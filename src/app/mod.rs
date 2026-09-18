use std::cell::{Cell, RefCell};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::agent::AgentEvent;
use crate::composer::Composer;
use crate::config::Config;
use crate::ollama::Client;
use crate::session::{AgentMode, Session, SessionMeta};
use crate::theme::Theme;
use crate::tools::{self, FileHit};
use crate::ui::{ChatCache, PlanCache};

mod input;
mod turn;

#[derive(Debug, Clone, Copy, Default)]
pub struct LayoutCache {
    pub body: Rect,
    pub max_scroll: u16,
    pub arrow_down: Option<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Welcome,
    Chat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Prompt,
    Scrollback,
    Todos,
}

pub enum Overlay {
    None,
    Help,
    Sessions(SessionsOverlay),
    Models {
        items: Vec<String>,
        selected: usize,
    },
    Permission {
        name: String,
        detail: String,
        selected: usize,
        reply: Option<oneshot::Sender<bool>>,
    },
    Question {
        prompt: String,
        hint: String,
        options: Vec<String>,
        selected: usize,
        draft: String,
        reply: Option<oneshot::Sender<String>>,
    },
    QuitConfirm,
    NewConfirm,
}

#[derive(Debug, Clone)]
pub struct SessionsOverlay {
    pub query: String,
    pub selected: usize,
    pub confirm_delete: bool,
    pub expanded: bool,
    pub filter_cwd: bool,
    pub searching: bool,
}

impl SessionsOverlay {
    pub fn open() -> Self {
        Self {
            query: String::new(),
            selected: 0,
            confirm_delete: false,
            expanded: true,
            filter_cwd: false,
            searching: false,
        }
    }

    fn step(&mut self, n: usize, down: bool) {
        step_index(&mut self.selected, n, down);
    }
}

impl Overlay {
    pub fn is_sessions(&self) -> bool {
        matches!(self, Self::Sessions(_))
    }

    pub fn sessions(&self) -> Option<&SessionsOverlay> {
        match self {
            Self::Sessions(s) => Some(s),
            _ => None,
        }
    }

    pub fn sessions_mut(&mut self) -> Option<&mut SessionsOverlay> {
        match self {
            Self::Sessions(s) => Some(s),
            _ => None,
        }
    }
}

pub(super) fn step_index(sel: &mut usize, n: usize, down: bool) {
    if n == 0 {
        return;
    }
    if down {
        *sel = (*sel + 1).min(n - 1);
    } else {
        *sel = sel.saturating_sub(1);
    }
}

pub struct App {
    pub cfg: Config,
    pub client: Client,
    pub theme: Theme,
    pub screen: Screen,
    pub session: Session,
    pub sessions: Vec<SessionMeta>,
    pub composer: Composer,
    pub focus: Focus,
    pub overlay: Overlay,
    /// Rows scrolled back from the bottom. A `Cell` so the draw pass can
    /// clamp it and keep the view anchored while rows stream in.
    pub scroll: Cell<u16>,
    pub follow: bool,
    pub layout: Cell<LayoutCache>,
    pub pick_hits: Cell<Vec<(Rect, usize)>>,
    pub close_hit: Cell<Option<Rect>>,
    pub selected_block: usize,
    pub show_todos: bool,
    pub toast: Option<(String, u8)>,
    pub tick: u64,
    pub connected: Option<Result<Vec<String>, String>>,
    pub slash_sel: usize,
    pub file_sel: usize,
    pub running: bool,
    pub cancel: Arc<AtomicBool>,
    /// In-flight turn. Stale bus events from an aborted turn are dropped.
    pub live_turn: u64,
    pub turn_task: Option<tokio::task::JoinHandle<()>>,
    pub tx: mpsc::UnboundedSender<(u64, AgentEvent)>,
    pub should_quit: bool,
    chord_esc: DoublePress,
    chord_q: DoublePress,
    chord_n: DoublePress,
    pub queue: Vec<String>,
    pub thinking_started: Option<Instant>,
    pub status_line: String,
    pub branch: String,
    /// Set whenever state changes; the event loop only redraws when it is on.
    pub dirty: bool,
    /// Escape-sequence tail that leaked in as plain characters after a lone ESC.
    pub(crate) residue: Option<Residue>,
    pub chat_cache: RefCell<ChatCache>,
    pub plan_cache: RefCell<PlanCache>,
    pub at_cache: RefCell<AtCache>,
}

/// `(started, inside CSI/SS3)` for `ESC [ … M` residue. See `App::eat_residue`.
pub(crate) struct Residue {
    started: Instant,
    in_seq: bool,
}

impl Residue {
    fn arm() -> Self {
        Self {
            started: Instant::now(),
            in_seq: false,
        }
    }
}

#[derive(Default)]
struct DoublePress {
    last: Option<Instant>,
}

impl DoublePress {
    fn hit(&mut self, window: Duration) -> bool {
        let now = Instant::now();
        if self.last.is_some_and(|t| now.duration_since(t) < window) {
            self.last = None;
            true
        } else {
            self.last = Some(now);
            false
        }
    }
}

#[derive(Default)]
pub struct AtCache {
    query: Option<String>,
    hits: Vec<FileHit>,
}

impl AtCache {
    fn hits(&mut self, cwd: &Path, query: &str) -> Vec<FileHit> {
        if self.query.as_deref() == Some(query) {
            return self.hits.clone();
        }
        self.hits = tools::list_at_level(cwd, query, 200);
        self.query = Some(query.to_string());
        self.hits.clone()
    }
}

impl App {
    pub fn new(cfg: Config, tx: mpsc::UnboundedSender<(u64, AgentEvent)>) -> anyhow::Result<Self> {
        let cwd = std::env::current_dir()?;
        let client = Client::from_config(&cfg);
        let mut session = Session::new(cwd.clone(), cfg.model().to_string());
        session.mode = if cfg.always_approve {
            AgentMode::Always
        } else {
            AgentMode::Normal
        };
        let sessions = Session::list(&cwd);
        let branch = git_branch(&cwd);
        let mut screen = Screen::Welcome;
        if std::env::var("ZODER_CONTINUE").ok().as_deref() == Some("1") {
            if let Some(meta) = sessions.first() {
                if let Ok(s) = Session::load(&meta.path) {
                    session = s;
                    screen = Screen::Chat;
                }
            }
        }
        Ok(Self {
            client,
            theme: Theme::night(),
            screen,
            session,
            sessions,
            composer: Composer::default(),
            focus: Focus::Prompt,
            overlay: Overlay::None,
            scroll: Cell::new(0),
            follow: true,
            layout: Cell::new(LayoutCache::default()),
            pick_hits: Cell::new(Vec::new()),
            close_hit: Cell::new(None),
            selected_block: 0,
            show_todos: false,
            toast: None,
            tick: 0,
            connected: None,
            slash_sel: 0,
            file_sel: 0,
            running: false,
            cancel: Arc::new(AtomicBool::new(false)),
            live_turn: 0,
            turn_task: None,
            tx,
            should_quit: false,
            chord_esc: DoublePress::default(),
            chord_q: DoublePress::default(),
            chord_n: DoublePress::default(),
            queue: Vec::new(),
            thinking_started: None,
            status_line: String::new(),
            branch,
            cfg,
            dirty: true,
            residue: None,
            chat_cache: RefCell::new(ChatCache::default()),
            plan_cache: RefCell::new(PlanCache::default()),
            at_cache: RefCell::new(AtCache::default()),
        })
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), 24));
        self.touch();
    }

    pub fn touch(&mut self) {
        self.dirty = true;
    }

    /// Redraw only when something changed. `on_tick` marks the frame dirty
    /// while work is animating, so an idle session draws zero frames.
    pub fn wants_draw(&self) -> bool {
        self.dirty
    }

    pub fn drawn(&mut self) {
        self.dirty = false;
    }

    pub fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        // The tick is fast (16ms) so a burst of stream deltas cannot outrun the
        // terminal; animation still advances on the old ~80ms cadence.
        if !self.tick.is_multiple_of(5) {
            return;
        }
        let mut draw = self.running;
        if let Some((_, n)) = self.toast.as_mut() {
            if *n == 0 {
                self.toast = None;
            } else {
                *n -= 1;
            }
            draw = true;
        }
        if self.tick.is_multiple_of(600) {
            draw = true;
        }
        if draw {
            self.touch();
        }
    }

    pub fn spinner(&self) -> char {
        const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        FRAMES[self.tick as usize % FRAMES.len()]
    }
}

pub(super) fn clock() -> String {
    chrono::Local::now().format("%I:%M %p").to_string()
}

pub fn cwd_label(cwd: &Path) -> String {
    cwd.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(".")
        .to_string()
}

fn git_branch(cwd: &Path) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "main".into())
}

#[cfg(test)]
impl App {
    pub fn demo() -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut cfg = Config::default();
        cfg.set_host("http://127.0.0.1:9".into());
        cfg.set_model("demo-model".into());
        let mut app = Self::new(cfg, tx).unwrap();
        let dir = tempfile::tempdir().expect("demo cwd");
        app.session.cwd = dir.path().to_path_buf();
        std::mem::forget(dir);
        app
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Block, ToolStatus};
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    #[test]
    fn submit_appends_turn() {
        let mut app = App::demo();
        app.composer.insert_str("hello from tests");
        app.submit();
        assert!(
            matches!(app.session.blocks.first(), Some(Block::User { text, .. }) if text == "hello from tests")
        );
        assert_eq!(app.screen, Screen::Chat);
    }

    #[test]
    fn mouse_tail_after_escape_is_swallowed() {
        let mut app = App::demo();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        for c in "[<64;149;19M".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.composer.text, "");
        app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        assert_eq!(app.composer.text, "h");
    }

    #[test]
    fn typing_after_escape_still_works() {
        let mut app = App::demo();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE));
        assert_eq!(app.composer.text, "a[");
    }

    #[test]
    fn control_chars_never_reach_the_composer() {
        let mut app = App::demo();
        app.handle_key(KeyEvent::new(KeyCode::Char('\u{1b}'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('\u{7}'), KeyModifiers::NONE));
        assert_eq!(app.composer.text, "");
    }

    #[test]
    fn dirty_flag_tracks_draws() {
        let mut app = App::demo();
        app.drawn();
        assert!(!app.wants_draw());
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(app.wants_draw());
        app.drawn();
        assert!(!app.wants_draw());
        // Idle ticks draw nothing.
        app.on_tick();
        assert!(!app.wants_draw());
        // A running turn advances the spinner every five ticks.
        app.running = true;
        for _ in 0..5 {
            app.on_tick();
        }
        assert!(app.wants_draw());
    }

    #[test]
    fn shift_tab_cycles_mode() {
        let mut app = App::demo();
        assert_eq!(app.session.mode, AgentMode::Normal);
        app.cycle_mode();
        assert_eq!(app.session.mode, AgentMode::Plan);
        app.cycle_mode();
        assert_eq!(app.session.mode, AgentMode::Always);
    }

    #[test]
    fn shift_enter_inserts_newline() {
        let mut app = App::demo();
        app.composer.insert_str("hello");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.composer.text, "hello\n");
        assert!(app.session.blocks.is_empty());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(app.composer.text, "hello\n\n");
    }

    #[test]
    fn enter_sends() {
        let mut app = App::demo();
        app.composer.insert_str("hello");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.composer.is_empty());
        assert!(
            matches!(app.session.blocks.first(), Some(Block::User { text, .. }) if text == "hello")
        );
    }

    #[test]
    fn trailing_backslash_enter_is_newline() {
        let mut app = App::demo();
        app.composer.insert_str("hello\\");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.composer.text, "hello\n");
        assert!(app.session.blocks.is_empty());
    }

    #[test]
    fn wheel_moves_one_line() {
        let mut app = App::demo();
        app.layout.set(LayoutCache {
            max_scroll: 40,
            ..LayoutCache::default()
        });
        app.scroll_transcript(1);
        assert_eq!(app.scroll.get(), 1);
        assert!(!app.follow);
        app.scroll_transcript(-1);
        assert_eq!(app.scroll.get(), 0);
        assert!(app.follow);
    }

    #[test]
    fn wheel_is_inert_when_there_is_nothing_to_scroll() {
        let mut app = App::demo();
        assert_eq!(app.layout.get().max_scroll, 0);
        for _ in 0..50 {
            app.scroll_transcript(1);
        }
        assert_eq!(
            app.scroll.get(),
            0,
            "must not bank an offset the draw pass would clamp away"
        );
        assert!(app.follow);
    }

    #[test]
    fn stored_scroll_never_exceeds_the_content() {
        let mut app = App::demo();
        app.layout.set(LayoutCache {
            max_scroll: 10,
            ..LayoutCache::default()
        });
        for _ in 0..40 {
            app.scroll_transcript(1);
        }
        assert_eq!(app.scroll.get(), 10);
    }

    #[test]
    fn motion_events_do_not_queue_a_redraw() {
        let mut app = App::demo();
        app.layout.set(LayoutCache {
            max_scroll: 30,
            ..LayoutCache::default()
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 3,
            row: 3,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.scroll.get(), 1, "wheel still scrolls the transcript");
        app.drawn();
        assert!(!app.wants_draw());
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 40,
            row: 12,
            modifiers: KeyModifiers::NONE,
        });
        assert!(!app.wants_draw(), "hover must not repaint");
        assert_eq!(app.scroll.get(), 1);
    }

    #[test]
    fn down_arrow_click_follows() {
        let mut app = App::demo();
        app.follow = false;
        app.scroll.set(12);
        app.layout.set(LayoutCache {
            max_scroll: 20,
            arrow_down: Some(Rect {
                x: 60,
                y: 33,
                width: 1,
                height: 1,
            }),
            ..LayoutCache::default()
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 60,
            row: 33,
            modifiers: KeyModifiers::NONE,
        });
        assert!(app.follow);
        assert_eq!(app.scroll.get(), 0);
    }

    #[test]
    fn slash_new_resets() {
        let mut app = App::demo();
        app.composer.insert_str("keep me");
        app.submit();
        app.running = false;
        app.composer.insert_str("/new");
        app.submit();
        assert_eq!(app.screen, Screen::Welcome);
        assert!(app.session.blocks.is_empty());
    }

    #[test]
    fn resume_keys_expand_filter_search() {
        use std::path::PathBuf;

        use chrono::Local;

        use crate::session::SessionMeta;

        let mut app = App::demo();
        app.session.cwd = PathBuf::from("/Users/a/Develop/zoder");
        let now = Local::now();
        app.sessions = vec![
            SessionMeta {
                id: "1".into(),
                title: "here".into(),
                updated: now,
                path: PathBuf::from("/tmp/1"),
                cwd: PathBuf::from("/Users/a/Develop/zoder"),
            },
            SessionMeta {
                id: "2".into(),
                title: "other project".into(),
                updated: now,
                path: PathBuf::from("/tmp/2"),
                cwd: PathBuf::from("/Users/a/Develop/assistant"),
            },
        ];
        app.overlay = Overlay::Sessions(SessionsOverlay::open());
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        assert!(!app.overlay.sessions().unwrap().expanded);
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert!(app.overlay.sessions().unwrap().filter_cwd);
        assert_eq!(app.picker_sessions().len(), 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert!(!app.overlay.sessions().unwrap().filter_cwd);
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
        let s = app.overlay.sessions().unwrap();
        assert!(s.searching);
        assert_eq!(s.query, "o");
        assert_eq!(app.picker_sessions().len(), 1);
        assert_eq!(app.picker_sessions()[0].title, "other project");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let s = app.overlay.sessions().unwrap();
        assert!(!s.searching);
        assert!(s.query.is_empty());
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(app.overlay.sessions().unwrap().confirm_delete);
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(!app.overlay.sessions().unwrap().confirm_delete);
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.overlay.sessions().unwrap().selected, 1);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn esc_cancels_a_running_turn() {
        let mut app = App::demo();
        app.running = true;
        app.live_turn = 3;
        app.session.blocks.push(Block::Tool {
            id: "1".into(),
            name: "bash".into(),
            detail: "sleep 30".into(),
            output: String::new(),
            status: ToolStatus::Running,
            folded: false,
            elapsed_ms: 0,
        });
        app.composer.insert_str("typed while working");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.running);
        assert!(app.cancel.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(app.composer.text, "typed while working");
        assert!(
            matches!(app.session.blocks.last(), Some(Block::Notice { text }) if text == "cancelled")
        );
        assert!(matches!(
            app.session.blocks.iter().find(|b| matches!(b, Block::Tool { .. })),
            Some(Block::Tool { status: ToolStatus::Failed, output, .. }) if output == "cancelled"
        ));
        app.on_agent(3, AgentEvent::ContentDelta("late".into()));
        assert!(!app
            .session
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Assistant { text, .. } if text.contains("late"))));
    }

    #[test]
    fn ctrl_c_clears_draft_without_stopping_the_turn() {
        let mut app = App::demo();
        app.running = true;
        app.composer.insert_str("keep going");
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.running);
        assert!(app.composer.is_empty());
    }

    #[test]
    fn ctrl_chords_ignore_input_language() {
        let mut app = App::demo();
        app.composer.insert_str("draft");
        app.handle_key(KeyEvent::new(KeyCode::Char('\u{3}'), KeyModifiers::NONE));
        assert!(app.composer.is_empty(), "ETX is Ctrl+C");
        app.composer.insert_str("draft");
        app.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL));
        assert!(app.composer.is_empty());
        app.composer.insert_str("draft");
        app.handle_key(KeyEvent::new(KeyCode::Char('中'), KeyModifiers::CONTROL));
        assert_eq!(
            app.composer.text, "draft",
            "Ctrl must not insert IME glyphs"
        );
        assert!(matches!(app.overlay, Overlay::None));
        if crate::layout::to_latin('แ') == Some('c') {
            app.composer.insert_str("draft");
            app.handle_key(KeyEvent::new(KeyCode::Char('แ'), KeyModifiers::CONTROL));
            assert!(
                app.composer.is_empty(),
                "physical C under the active layout"
            );
        }
    }
}
