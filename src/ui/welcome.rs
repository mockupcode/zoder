use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;

pub(super) fn draw_welcome(frame: &mut Frame, app: &App, area: Rect) {
    let th = app.theme;
    // Wide-terminal capture: card width caps at 120 and is centered.
    // 120-col: x=3 width=114. 160-col: x=20 width=120. 200-col: x=40 width=120.
    let card_h = 13u16.min(area.height.saturating_sub(2));
    let card_w = 120u16.min(area.width.saturating_sub(6));
    let top_gap = 5u16.min(area.height.saturating_sub(card_h));
    let card = Rect {
        x: area.x + (area.width.saturating_sub(card_w)) / 2,
        y: area.y + top_gap,
        width: card_w,
        height: card_h,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(th.border).bg(th.bg))
        .style(th.base());
    let inner = block.inner(card);
    frame.render_widget(block, card);

    let status = match &app.connected {
        Some(Ok(_)) => ("connected", th.gold),
        Some(Err(_)) => ("offline", th.red),
        None => ("connecting…", th.gold),
    };
    let ver = env!("CARGO_PKG_VERSION");
    let inner_w = inner.width as usize;
    // Desktop z.png → 52x36 dots → 26 cols × 9 rows.
    let figure = [
        "⠀⠀⠈⢿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠟⠀⠀⠀⠀⠀⠀⠀⠀",
        "⠀⠀⠀⠈⠿⠿⠿⠿⠿⠿⠿⢿⣿⣿⣿⡿⠃⠀⠀⠀⠀⠀⣀⣀⠀⠀",
        "⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠰⠿⠛⠛⣋⣁⣤⡤⠶⠒⠋⡉⠁⠀⠀⠀",
        "⠀⠀⠀⠀⠀⠀⠀⢀⣀⣤⣴⣶⡿⠿⠛⣉⣡⡴⠖⠛⠉⠀⠀⠀⠀⠀",
        "⠀⠀⠀⣀⣤⣶⣾⣿⡿⠟⢋⣡⣴⣾⠿⠋⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀",
        "⠀⣤⣾⣿⣿⠿⠋⣁⣴⣾⣿⠿⠋⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀",
        "⣾⣿⣿⠟⠁⣠⣾⣿⣿⠟⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀",
        "⠻⣿⠃⠀⣾⣿⣿⣿⣷⣶⣶⣶⣶⣶⣶⣶⣄⠀⠀⠀⠀⠀⠀⠀⠀⠀",
        "⠀⠙⠆⠸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣦⠀⠀⠀⠀⠀⠀⠀⠀",
    ];
    let fig = Style::default().fg(th.logo).bg(th.bg);
    let key = Style::default().fg(th.fg_gray).bg(th.bg);
    let menu = |fig_row: &str, label: &str, shortcut: &str| -> Line {
        let left_w = 2
            + unicode_width::UnicodeWidthStr::width(fig_row)
            + 2
            + unicode_width::UnicodeWidthStr::width(label);
        let gap = inner_w.saturating_sub(left_w + unicode_width::UnicodeWidthStr::width(shortcut));
        Line::from(vec![
            Span::styled("  ", th.base()),
            Span::styled(fig_row.to_string(), fig),
            Span::styled("  ", th.base()),
            Span::styled(label.to_string(), th.base()),
            Span::styled(" ".repeat(gap), th.base()),
            Span::styled(shortcut.to_string(), key),
        ])
    };
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  ", th.base()),
            Span::styled(figure[0], fig),
            Span::styled("  Zoder  ", th.base().add_modifier(Modifier::BOLD)),
            Span::styled(ver, th.dim()),
        ]),
        Line::from(vec![
            Span::styled("  ", th.base()),
            Span::styled(figure[1], fig),
        ]),
        Line::from(vec![
            Span::styled("  ", th.base()),
            Span::styled(figure[2], fig),
            Span::styled("  ", th.base()),
            Span::styled(
                status.0.to_string(),
                Style::default().fg(status.1).bg(th.bg),
            ),
        ]),
        Line::from(vec![
            Span::styled("  ", th.base()),
            Span::styled(figure[3], fig),
            Span::styled("  ", th.base()),
            Span::styled(
                format!("{}  {}", app.cfg.host(), app.session.model),
                th.dim(),
            ),
        ]),
        Line::from(vec![
            Span::styled("  ", th.base()),
            Span::styled(figure[4], fig),
        ]),
        menu(figure[5], "New session", "ctrl+n  "),
        menu(figure[6], "Resume session", "ctrl+r  "),
        Line::from(vec![
            Span::styled("  ", th.base()),
            Span::styled(figure[7], fig),
        ]),
        menu(figure[8], "Quit", "ctrl+q  "),
        Line::from(""),
    ];
    frame.render_widget(Paragraph::new(lines).style(th.base()), inner);

    if area.height > card_h {
        let tip = Rect {
            x: 3,
            y: area.y + area.height.saturating_sub(1),
            width: area.width.saturating_sub(3),
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Span::styled("Tip: Type a task and press enter.", th.dim())),
            tip,
        );
    }
}
