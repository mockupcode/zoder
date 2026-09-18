use std::cell::Cell;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use ratatui::layout::Rect;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::agent::AgentEvent;
use crate::composer::Composer;
use crate::config::Config;
use crate::ollama::Client;
use crate::session::{AgentMode, Session, SessionMeta};
use crate::theme::Theme;


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
    Sessions {
        query: String,
        selected: usize,
        confirm_delete: bool,
        expanded: bool,
        filter_cwd: bool,
        searching: bool,
    },
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
    QuitConfirm,
    NewConfirm,
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
    pub scroll: u16,
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
    pub tx: mpsc::UnboundedSender<AgentEvent>,
    pub should_quit: bool,
    pub last_esc: Option<Instant>,
    pub last_ctrl_q: Option<Instant>,
    pub last_ctrl_n: Option<Instant>,
    pub queue: Vec<String>,
    pub thinking_started: Option<Instant>,
    pub status_line: String,
    pub branch: String,
}

impl App {
    pub fn new(cfg: Config, tx: mpsc::UnboundedSender<AgentEvent>) -> anyhow::Result<Self> {
        let cwd = std::env::current_dir()?;
        let client = Client::from_config(&cfg);
        let mut session = Session::new(cwd.clone(), cfg.model().to_string());
        session.mode = if cfg.always_approve {
            AgentMode::Always
        } else {
            AgentMode::Normal
        };
        let sessions = Session::list(&cwd);
        let branch = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&cwd)
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
            .unwrap_or_else(|| "main".into());
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
            scroll: 0,
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
            tx,
            should_quit: false,
            last_esc: None,
            last_ctrl_q: None,
            last_ctrl_n: None,
            queue: Vec::new(),
            thinking_started: None,
            status_line: String::new(),
            branch,
            cfg,
        })
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), 24));
    }

    pub fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        if let Some((_, n)) = self.toast.as_mut() {
            if *n == 0 {
                self.toast = None;
            } else {
                *n -= 1;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Block;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    fn demo() -> App {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut cfg = Config::default();
        cfg.set_host("http://127.0.0.1:9".into());
        cfg.set_model("demo-model".into());
        App::new(cfg, tx).unwrap()
    }

    #[test]
    fn submit_appends_turn() {
        let mut app = demo();
        app.composer.insert_str("hello from tests");
        app.submit();
        assert!(
            matches!(app.session.blocks.first(), Some(Block::User { text, .. }) if text == "hello from tests")
        );
        assert_eq!(app.screen, Screen::Chat);
    }

    #[test]
    fn shift_tab_cycles_mode() {
        let mut app = demo();
        assert_eq!(app.session.mode, AgentMode::Normal);
        app.cycle_mode();
        assert_eq!(app.session.mode, AgentMode::Plan);
        app.cycle_mode();
        assert_eq!(app.session.mode, AgentMode::Always);
    }

    #[test]
    fn shift_enter_inserts_newline() {
        let mut app = demo();
        app.composer.insert_str("hello");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.composer.text, "hello\n");
        assert!(app.session.blocks.is_empty());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(app.composer.text, "hello\n\n");
    }

    #[test]
    fn enter_sends() {
        let mut app = demo();
        app.composer.insert_str("hello");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.composer.is_empty());
        assert!(
            matches!(app.session.blocks.first(), Some(Block::User { text, .. }) if text == "hello")
        );
    }

    #[test]
    fn trailing_backslash_enter_is_newline() {
        let mut app = demo();
        app.composer.insert_str("hello\\");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.composer.text, "hello\n");
        assert!(app.session.blocks.is_empty());
    }

    #[test]
    fn wheel_moves_one_line() {
        let mut app = demo();
        app.layout.set(LayoutCache {
            max_scroll: 40,
            ..LayoutCache::default()
        });
        app.scroll_transcript(1);
        assert_eq!(app.scroll, 1);
        assert!(!app.follow);
        app.scroll_transcript(-1);
        assert_eq!(app.scroll, 0);
        assert!(app.follow);
    }

    #[test]
    fn down_arrow_click_follows() {
        let mut app = demo();
        app.follow = false;
        app.scroll = 12;
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
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn slash_new_resets() {
        let mut app = demo();
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

        let mut app = demo();
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
        app.overlay = Overlay::Sessions {
            query: String::new(),
            selected: 0,
            confirm_delete: false,
            expanded: true,
            filter_cwd: false,
            searching: false,
        };
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        match &app.overlay {
            Overlay::Sessions { expanded, .. } => assert!(!*expanded),
            _ => panic!("overlay"),
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        match &app.overlay {
            Overlay::Sessions { filter_cwd, .. } => assert!(*filter_cwd),
            _ => panic!("overlay"),
        }
        assert_eq!(app.picker_sessions().len(), 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        match &app.overlay {
            Overlay::Sessions { filter_cwd, .. } => assert!(!*filter_cwd),
            _ => panic!("overlay"),
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
        match &app.overlay {
            Overlay::Sessions {
                query, searching, ..
            } => {
                assert!(*searching);
                assert_eq!(query, "o");
            }
            _ => panic!("overlay"),
        }
        assert_eq!(app.picker_sessions().len(), 1);
        assert_eq!(app.picker_sessions()[0].title, "other project");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        match &app.overlay {
            Overlay::Sessions {
                query, searching, ..
            } => {
                assert!(!*searching);
                assert!(query.is_empty());
            }
            _ => panic!("overlay"),
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        match &app.overlay {
            Overlay::Sessions { confirm_delete, .. } => assert!(*confirm_delete),
            _ => panic!("overlay"),
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        match &app.overlay {
            Overlay::Sessions { confirm_delete, .. } => assert!(!*confirm_delete),
            _ => panic!("overlay"),
        }
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        match &app.overlay {
            Overlay::Sessions { selected, .. } => assert_eq!(*selected, 1),
            _ => panic!("overlay"),
        }
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.overlay, Overlay::None));
    }
}
