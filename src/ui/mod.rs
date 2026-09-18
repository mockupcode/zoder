use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::app::{App, LayoutCache, Screen};
use crate::text::wrap_plain;

mod chat;
pub(crate) use chat::{ChatCache, PlanCache};
mod chrome;
mod overlays;
mod welcome;

use chat::draw_chat;
use chrome::{draw_composer, draw_dropdowns, draw_footer, draw_header};
use overlays::draw_overlay;
use welcome::draw_welcome;

pub fn draw(frame: &mut Frame, app: &App) {
    let th = app.theme;
    frame.render_widget(Block::default().style(th.base()), frame.area());
    if frame.area().width < 48 || frame.area().height < 12 {
        frame.render_widget(
            Paragraph::new("widen the terminal")
                .style(th.dim())
                .alignment(Alignment::Center),
            frame.area(),
        );
        return;
    }

    // 40-row capture: composer at rows 34-36 (height-6), shortcuts at 38 (height-2),
    // one blank under composer and one blank on the last row.
    let ch = composer_height(app, frame.area().width.saturating_sub(4));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(1),
            Constraint::Length(ch),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_header(frame, app, chunks[1]);
    let resume = app.overlay.is_sessions();
    if !resume {
        match app.screen {
            Screen::Welcome => {
                app.layout.set(LayoutCache::default());
                draw_welcome(frame, app, chunks[2]);
            }
            Screen::Chat => draw_chat(frame, app, chunks[2], chunks[3]),
        }
        draw_dropdowns(frame, app, chunks[4]);
    } else {
        app.layout.set(LayoutCache::default());
    }
    draw_composer(frame, app, inset(chunks[4], 2));
    if !resume || app.toast.is_some() {
        draw_footer(frame, app, chunks[6]);
    }
    draw_overlay(frame, app);
}

pub(super) fn inset(area: Rect, hpad: u16) -> Rect {
    Rect {
        x: area.x + hpad,
        y: area.y,
        width: area.width.saturating_sub(hpad * 2),
        height: area.height,
    }
}

fn composer_height(app: &App, inner_width: u16) -> u16 {
    let inner = inner_width.saturating_sub(4).max(8) as usize;
    let lines = wrap_plain(&app.composer.text, inner).len().max(1);
    (lines.min(8) as u16) + 2
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::app::{Overlay, SessionsOverlay};
    use crate::session::{Block, ToolStatus};

    fn demo() -> App {
        App::demo()
    }

    fn shot(app: &App, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn welcome_shows_layout() {
        let app = demo();
        let s = shot(&app, 120, 36);
        assert!(s.contains("❯"), "{s}");
        assert!(s.contains("╭"), "{s}");
        assert!(s.contains("Zoder"), "{s}");
        assert!(s.contains("Resume session"), "{s}");
        assert!(s.contains("demo-model"), "{s}");
        assert!(s.contains("Shift+Tab:mode"), "{s}");
        assert!(s.contains("Ctrl+x:shortcuts"), "{s}");
        assert!(!s.contains("Enter:send"), "{s}");
        assert!(!s.to_lowercase().contains(concat!("gr", "ok")));
        assert!(!s.to_lowercase().contains(concat!("x", "ai")));
    }

    fn first_transcript_row(s: &str) -> String {
        s.lines()
            .find(|l| l.contains("row-"))
            .unwrap_or("")
            .trim()
            .to_string()
    }

    fn push_rows(app: &mut App, range: impl Iterator<Item = usize>) {
        for i in range {
            app.session.blocks.push(Block::User {
                text: format!("row-{i:02}"),
                time: String::new(),
            });
        }
    }

    #[test]
    fn view_stays_anchored_while_rows_stream_in() {
        let mut app = demo();
        app.screen = Screen::Chat;
        push_rows(&mut app, 0..30);
        let _ = shot(&app, 100, 24);
        app.follow = false;
        app.scroll.set(5);
        let parked = first_transcript_row(&shot(&app, 100, 24));
        push_rows(&mut app, 30..34);
        let after = first_transcript_row(&shot(&app, 100, 24));
        assert_eq!(
            after, parked,
            "rows appended below must not slide the view under the reader"
        );
    }

    #[test]
    fn transcript_cache_follows_new_blocks_and_scroll() {
        let mut app = demo();
        app.screen = Screen::Chat;
        push_rows(&mut app, 0..30);
        let first = shot(&app, 100, 24);
        assert!(first.contains("row-29"), "{first}");
        app.session.blocks.push(Block::User {
            text: "row-30-fresh".into(),
            time: String::new(),
        });
        let second = shot(&app, 100, 24);
        assert!(
            second.contains("row-30-fresh"),
            "new block must invalidate the cache\n{second}"
        );
        app.scroll.set(3);
        let third = shot(&app, 100, 24);
        assert!(
            !third.contains("row-30-fresh") && third.contains("row-29"),
            "scrolling must move the visible window\n{third}"
        );
    }

    #[test]
    fn chat_shows_user_and_tool() {
        let mut app = demo();
        app.screen = Screen::Chat;
        app.session.blocks.push(crate::session::Block::User {
            text: "inspect the crate".into(),
            time: String::new(),
        });
        app.session.blocks.push(crate::session::Block::Tool {
            id: "1".into(),
            name: "read_file".into(),
            detail: "Cargo.toml".into(),
            output: "[package]\nname = \"zoder\"\n".into(),
            status: ToolStatus::Ok,
            folded: false,
            elapsed_ms: 12,
        });
        let s = shot(&app, 120, 36);
        assert!(s.contains("❯"), "{s}");
        assert!(s.contains("inspect the crate"), "{s}");
        assert!(s.contains("Read"), "{s}");
        assert!(s.contains("Cargo.toml"), "{s}");
        assert!(s.contains("Shift+Tab:mode"), "{s}");
        assert!(!s.contains("Enter:send"), "{s}");
    }

    #[test]
    fn overlay_cards_share_resume_chrome() {
        let mut app = demo();
        app.overlay = Overlay::Help;
        let s = shot(&app, 120, 36);
        assert!(s.contains("┌─"), "{s}");
        assert!(s.contains("shortcuts"), "{s}");
        assert!(s.contains("[✗]"), "{s}");
        assert!(s.contains("esc close") || s.contains("esc  close"), "{s}");
        app.overlay = Overlay::QuitConfirm;
        let s = shot(&app, 120, 36);
        assert!(s.contains("Quit?"), "{s}");
        assert!(s.contains("[✗]"), "{s}");
        assert!(s.contains("enter confirm"), "{s}");
    }

    #[test]
    fn footer_hints_follow_composer_state() {
        let mut app = demo();
        let empty = shot(&app, 120, 36);
        assert!(
            empty.contains("Shift+Tab:mode | Ctrl+x:shortcuts"),
            "{empty}"
        );
        assert!(!empty.contains("Enter:send"), "{empty}");
        app.composer.insert_str("hello");
        let typed = shot(&app, 120, 36);
        assert!(
            typed.contains("Enter:send | Shift+Enter:newline | Shift+Tab:mode | Ctrl+x:shortcuts"),
            "{typed}"
        );
        app.composer.clear();
        app.composer.insert('/');
        let slash = shot(&app, 120, 36);
        assert!(slash.contains("Enter:send"), "{slash}");
        assert!(slash.contains("Shift+Enter:newline"), "{slash}");
    }

    fn resume_demo() -> App {
        use std::path::PathBuf;

        use chrono::{Duration, Local};

        use crate::session::SessionMeta;

        let mut app = demo();
        app.session.cwd = PathBuf::from("/Users/a/Develop/zoder");
        app.session.model = "demo-model".into();
        let now = Local::now();
        app.sessions = vec![
            SessionMeta {
                id: "1".into(),
                title: "Rust TUI matched to captured reference palette".into(),
                updated: now,
                path: PathBuf::from("/tmp/1"),
                cwd: PathBuf::from("/Users/a/Develop/zoder"),
            },
            SessionMeta {
                id: "2".into(),
                title: "IoT pump off claimed without tool call".into(),
                updated: now - Duration::hours(72),
                path: PathBuf::from("/tmp/2"),
                cwd: PathBuf::from("/Users/a/Develop/assistant"),
            },
            SessionMeta {
                id: "3".into(),
                title: "Pixel Art 2.5D Village Scene Game".into(),
                updated: now - Duration::hours(24 * 23),
                path: PathBuf::from("/tmp/3"),
                cwd: PathBuf::from("/Users/a/Develop/tycoon"),
            },
        ];
        app.overlay = Overlay::Sessions(SessionsOverlay::open());
        app
    }

    #[test]
    fn resume_matches_screenshot_layout() {
        let app = resume_demo();
        let s = shot(&app, 200, 40);
        assert!(s.contains("Resume session"), "{s}");
        assert!(s.contains("[✗]"), "{s}");
        assert!(s.contains("/ to search"), "{s}");
        assert!(s.contains("All f"), "{s}");
        assert!(s.contains("Develop-zoder"), "{s}");
        assert!(s.contains("Develop-assistant"), "{s}");
        assert!(s.contains("Develop-tycoon"), "{s}");
        let a = s.find("Develop-assistant").unwrap();
        let t = s.find("Develop-tycoon").unwrap();
        let z = s.find("Develop-zoder").unwrap();
        assert!(a < t && t < z, "groups must be alphabetical\n{s}");
        assert!(s.contains("IoT pump off claimed without tool call"), "{s}");
        assert!(s.contains("just now"), "{s}");
        assert!(s.contains("3d ago"), "{s}");
        assert!(s.contains("↑↓"), "{s}");
        assert!(s.contains("e expand"), "{s}");
        assert!(s.contains("/ search"), "{s}");
        assert!(s.contains("f filter"), "{s}");
        assert!(s.contains("d delete"), "{s}");
        assert!(s.contains("demo-model · normal"), "{s}");
        assert!(s.contains("┌"), "{s}");
        assert!(s.contains("└"), "{s}");
        assert!(s.contains("› Rust TUI") || s.contains("› IoT"), "{s}");
        let top_line = s.lines().find(|l| l.contains('┌')).expect("top");
        let start = top_line.find('┌').expect("┌");
        let end = top_line.rfind('┐').expect("┐") + '┐'.len_utf8();
        let piece = &top_line[start..end];
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(piece),
            120,
            "{top_line}"
        );
        assert!(
            s.lines()
                .any(|l| l.contains("Resume session") && l.contains("[✗]") && l.contains('┌')),
            "{s}"
        );
        assert!(!s.contains("New session"), "{s}");
        assert!(!s.contains("Hope you are"), "{s}");
        assert!(!s.contains("From the team"), "{s}");
        assert!(s.contains("❯"), "{s}");
        let top = s
            .lines()
            .find(|l| l.contains("Resume session"))
            .unwrap_or("");
        let search = s.lines().find(|l| l.contains("/ to search")).unwrap_or("");
        assert!(
            top.find("Resume session").unwrap() < top.find("[✗]").unwrap(),
            "{s}"
        );
        assert!(
            search.find("/ to search").unwrap() < search.find('f').unwrap_or(0),
            "{s}"
        );
        assert!(
            s.lines().any(|l| l.contains('╭') && l.contains("e expand")),
            "footer sits on the composer top border\n{s}"
        );
        assert!(
            s.lines()
                .any(|l| l.contains('└') && (l.contains('❯') || l.contains('│'))),
            "box bottom sits on the composer prompt row\n{s}"
        );
    }

    #[test]
    fn resume_box_is_120_on_wide_terminal() {
        let app = resume_demo();
        let s = shot(&app, 272, 61);
        let top = s.lines().find(|l| l.contains('┌')).expect("top");
        let start = top.find('┌').expect("┌");
        let end = top.rfind('┐').expect("┐") + '┐'.len_utf8();
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(&top[start..end]),
            120,
            "screenshot terminal is ~272 cols; box stays 120 centered, not full width\n{top}"
        );
        let leading = top.find('┌').unwrap();
        assert!(leading > 40, "box must be centered, not full-bleed\n{top}");
        assert!(
            s.lines().any(|l| l.contains('╭') && l.contains("e expand")),
            "at 61 rows the box footer lands on the composer top\n{s}"
        );
        assert!(
            s.lines().any(|l| l.contains('└') && l.contains('❯')),
            "at 61 rows the box bottom lands on the prompt row\n{s}"
        );
    }

    #[test]
    fn resume_filter_chip_toggles_all_local() {
        let mut app = resume_demo();
        assert!(shot(&app, 200, 40).contains("All f"));
        if let Some(s) = app.overlay.sessions_mut() {
            s.filter_cwd = true;
        }
        let s = shot(&app, 200, 40);
        assert!(s.contains("Local f"), "{s}");
        assert!(!s.contains("All f"), "{s}");
    }

    #[test]
    fn resume_hides_welcome_card() {
        let app = resume_demo();
        let s = shot(&app, 200, 40);
        assert!(!s.contains("ctrl+n"), "{s}");
        assert!(!s.contains("Tip: Type a task"), "{s}");
    }

    #[test]
    fn resume_selected_row_is_full_width_bar() {
        use ratatui::style::Color;

        let app = resume_demo();
        let backend = TestBackend::new(200, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        let mut bar = 0u16;
        for y in 0..40u16 {
            let mut row = String::new();
            let mut n = 0u16;
            for x in 0..200u16 {
                row.push_str(buf[(x, y)].symbol());
                if buf[(x, y)].bg == Color::Rgb(54, 54, 54) {
                    n += 1;
                }
            }
            if row.contains("IoT pump off claimed") {
                bar = n;
                break;
            }
        }
        assert!(
            (110..=116).contains(&bar),
            "highlight is inner width minus 2-col inset each side, got {bar}"
        );
    }

    #[test]
    fn resume_text_colors_match_screenshot() {
        use ratatui::style::Color;

        let app = resume_demo();
        let backend = TestBackend::new(200, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        let mut title_fg = None;
        let mut search_fg = None;
        let mut group_fg = None;
        let mut dash_fg = None;
        let mut sess_fg = None;
        for y in 0..40u16 {
            let mut row = String::new();
            for x in 0..200u16 {
                row.push_str(buf[(x, y)].symbol());
            }
            for x in 0..200u16 {
                let cell = &buf[(x, y)];
                let ch = cell.symbol();
                if title_fg.is_none() && row.contains("Resume session") && ch == "R" {
                    let mut word = String::new();
                    for i in 0..6u16 {
                        word.push_str(buf[(x + i, y)].symbol());
                    }
                    if word.starts_with("Resume") {
                        title_fg = Some(cell.fg);
                    }
                }
                if search_fg.is_none() && row.contains("/ to search") && ch == "/" {
                    search_fg = Some(cell.fg);
                }
                if group_fg.is_none() && row.contains("Develop-zoder") && ch == "D" {
                    let mut word = String::new();
                    for i in 0..7u16 {
                        word.push_str(buf[(x + i, y)].symbol());
                    }
                    if word.starts_with("Develop") {
                        group_fg = Some(cell.fg);
                    }
                }
                if dash_fg.is_none()
                    && row.contains("Develop-zoder")
                    && ch == "─"
                    && cell.fg == Color::Rgb(88, 88, 88)
                {
                    dash_fg = Some(cell.fg);
                }
                if sess_fg.is_none() && row.contains("IoT pump") && ch == "I" {
                    sess_fg = Some(cell.fg);
                }
            }
        }
        assert_eq!(title_fg, Some(Color::Rgb(225, 225, 225)), "title");
        assert_eq!(search_fg, Some(Color::Rgb(88, 88, 88)), "search");
        assert_eq!(group_fg, Some(Color::Rgb(108, 108, 108)), "group");
        assert_eq!(dash_fg, Some(Color::Rgb(88, 88, 88)), "dashes");
        assert_eq!(sess_fg, Some(Color::Rgb(225, 225, 225)), "session");

        let mut chev_fg = None;
        let mut f_fg = None;
        for y in 0..40u16 {
            let mut row = String::new();
            for x in 0..200u16 {
                row.push_str(buf[(x, y)].symbol());
            }
            for x in 0..200u16 {
                let cell = &buf[(x, y)];
                if chev_fg.is_none() && row.contains("› IoT") && cell.symbol() == "›" {
                    chev_fg = Some(cell.fg);
                }
                if f_fg.is_none() && row.contains("/ to search") && row.contains("All f") {
                    // last 'f' on the search row is the filter key
                    if cell.symbol() == "f" {
                        f_fg = Some(cell.fg);
                    }
                }
            }
        }
        assert_eq!(chev_fg, Some(Color::Rgb(108, 108, 108)), "chevron");
        assert_eq!(f_fg, Some(Color::Rgb(88, 88, 88)), "filter f");
    }

    #[test]
    fn slash_menu_matches_screenshot() {
        let mut app = demo();
        app.screen = Screen::Chat;
        app.composer.insert('/');
        let s = shot(&app, 120, 36);
        assert!(s.contains("/new"), "{s}");
        assert!(s.contains("Start a fresh session"), "{s}");
        assert!(s.contains("❯ /new"), "{s}");
        let slash_cols: Vec<usize> = s
            .lines()
            .filter_map(|l| {
                let i = if l.contains("> /new") || l.contains("❯ /new") {
                    l.find("/new")
                } else if l.contains("/resume") && l.contains("session picker") {
                    l.find("/resume")
                } else if l.contains("/clear") && l.contains("Alias") {
                    l.find("/clear")
                } else {
                    None
                }?;
                Some(unicode_width::UnicodeWidthStr::width(&l[..i]))
            })
            .collect();
        assert!(
            slash_cols.len() >= 2 && slash_cols.iter().all(|c| *c == slash_cols[0]),
            "command names must share one column {slash_cols:?}\n{s}"
        );
        assert!(
            s.lines()
                .any(|l| l.contains("/resume") && l.contains("Open the session picker")),
            "{s}"
        );
        assert!(!s.contains(" commands "), "{s}");
        assert!(s.contains("/always-approve") || s.contains("/quit"), "{s}");

        app.composer.clear();
        app.composer.insert_str("/as");
        let s = shot(&app, 120, 36);
        assert!(s.contains("/always-approve"), "{s}");
        assert!(!s.contains("/new") || s.contains("/always-approve"), "{s}");

        use ratatui::style::Color;
        let backend = TestBackend::new(120, 36);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        let mut typed_fg = None;
        for y in 0..36u16 {
            for x in 0..120u16 {
                if buf[(x, y)].symbol() == "a" {
                    let mut word = String::new();
                    for i in 0..2u16 {
                        if x + i < 120 {
                            word.push_str(buf[(x + i, y)].symbol());
                        }
                    }
                    if word == "as" {
                        typed_fg = Some(buf[(x, y)].fg);
                    }
                }
            }
        }
        assert_eq!(
            typed_fg,
            Some(Color::Rgb(110, 163, 254)),
            "typed query stays blue while matches exist"
        );
    }
}
