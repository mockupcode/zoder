use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Overlay, SessionsOverlay};
use crate::session::Session;
use crate::text::{truncate_width, wrap_plain};
use crate::theme::Theme;

pub(super) fn draw_overlay(frame: &mut Frame, app: &App) {
    let th = app.theme;
    match &app.overlay {
        Overlay::None => {}
        Overlay::Help => help(frame, app, th),
        Overlay::QuitConfirm => confirm(
            frame,
            app,
            th,
            "Quit?",
            "press ctrl+q again or enter · esc to stay",
        ),
        Overlay::NewConfirm => confirm(
            frame,
            app,
            th,
            "New session?",
            "press ctrl+n again or enter · discards nothing already saved",
        ),
        Overlay::Sessions(s) => sessions(frame, app, s),
        Overlay::Models { items, selected } => models(frame, app, th, items, *selected),
        Overlay::Question {
            prompt,
            hint,
            options,
            selected,
            draft,
            ..
        } => question(frame, app, th, prompt, hint, options, *selected, draft),
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn overlay_top(width: usize, title: &str, th: Theme) -> Line<'static> {
    let border = Style::default().fg(th.fg_mute).bg(th.bg);
    let title_st = Style::default()
        .fg(th.fg)
        .bg(th.bg)
        .add_modifier(Modifier::BOLD);
    let left = "┌─ ";
    let name = title.to_string();
    let right = " [✗] ─┐";
    let fill = width.saturating_sub(left.width() + name.width() + 1 + right.width());
    Line::from(vec![
        Span::styled(left, border),
        Span::styled(name, title_st),
        Span::styled(" ", border),
        Span::styled("─".repeat(fill), border),
        Span::styled(right, border),
    ])
}

fn overlay_bottom(inner_w: usize, border: Style) -> Line<'static> {
    Line::from(Span::styled(format!("└{}┘", "─".repeat(inner_w)), border))
}

fn overlay_keys(
    inner_w: usize,
    keys: &[(&'static str, &'static str)],
    th: Theme,
    border: Style,
) -> Line<'static> {
    let key = Style::default().fg(th.fg_bright).bg(th.bg);
    let lab = Style::default().fg(th.fg_dim).bg(th.bg);
    let mut mid: Vec<Span> = Vec::new();
    for (i, (k, l)) in keys.iter().enumerate() {
        if i > 0 {
            mid.push(Span::styled("  |  ", lab));
        }
        mid.push(Span::styled((*k).to_string(), key));
        mid.push(Span::styled(format!(" {l}"), lab));
    }
    let w = crate::text::spans_width(&mid);
    let left = inner_w.saturating_sub(w) / 2;
    let right = inner_w.saturating_sub(left + w);
    let mut spans = vec![
        Span::styled("│", border),
        Span::styled(" ".repeat(left), th.base()),
    ];
    spans.append(&mut mid);
    spans.push(Span::styled(" ".repeat(right), th.base()));
    spans.push(Span::styled("│", border));
    Line::from(spans)
}

fn paint_card(
    frame: &mut Frame,
    app: &App,
    th: Theme,
    title: &str,
    box_w: u16,
    body: Vec<(Line<'static>, bool)>,
    keys: &[(&'static str, &'static str)],
) {
    let border = Style::default().fg(th.fg_mute).bg(th.bg);
    let box_h = (body.len() as u16).saturating_add(4).max(6);
    let area = centered(frame.area(), box_w, box_h);
    let inner_w = area.width.saturating_sub(2) as usize;
    frame.render_widget(Clear, area);
    let mut lines = vec![overlay_top(area.width as usize, title, th)];
    lines.push(sided(Line::from(""), inner_w, th, border, false));
    for (line, selected) in body {
        lines.push(sided(line, inner_w, th, border, selected));
    }
    while lines.len() + 2 < area.height as usize {
        lines.push(sided(Line::from(""), inner_w, th, border, false));
    }
    lines.push(overlay_keys(inner_w, keys, th, border));
    lines.push(overlay_bottom(inner_w, border));
    if lines.len() > area.height as usize {
        lines.truncate(area.height as usize);
        if let Some(last) = lines.last_mut() {
            *last = overlay_bottom(inner_w, border);
        }
    }
    frame.render_widget(Paragraph::new(lines).style(th.base()), area);
    app.close_hit.set(Some(Rect {
        x: area.x + area.width.saturating_sub(6),
        y: area.y,
        width: 3,
        height: 1,
    }));
}

fn help(frame: &mut Frame, app: &App, th: Theme) {
    let body = vec![
        Line::from(Span::styled("  Keyboard", th.accent_bold())),
        Line::from(""),
        Line::from("  enter            send (or queue while working)"),
        Line::from("  tab              prompt ↔ scrollback"),
        Line::from("  esc              cancel a running turn"),
        Line::from("  ctrl+c           clear draft"),
        Line::from("  ctrl+n n         new session"),
        Line::from("  ctrl+r           resume a session"),
        Line::from("  ctrl+t           todos pane"),
        Line::from("  ctrl+m           models (or multiline in the prompt)"),
        Line::from("  !                shell on an empty prompt"),
        Line::from("  /                commands"),
        Line::from("  @                attach a file path"),
        Line::from("  esc esc          clear draft"),
        Line::from("  ctrl+q q         quit"),
    ];
    let body: Vec<(Line, bool)> = body.into_iter().map(|l| (l, false)).collect();
    paint_card(frame, app, th, "shortcuts", 72, body, &[("esc", "close")]);
}

fn confirm(frame: &mut Frame, app: &App, th: Theme, title: &str, body: &str) {
    paint_card(
        frame,
        app,
        th,
        title,
        56,
        vec![
            (Line::from(""), false),
            (
                Line::from(Span::styled(format!("  {body}"), th.dim())),
                false,
            ),
        ],
        &[("enter", "confirm"), ("esc", "cancel")],
    );
}

fn sessions(frame: &mut Frame, app: &App, picker: &SessionsOverlay) {
    let th = app.theme;
    let full = frame.area();
    let border = Style::default().fg(th.fg_mute).bg(th.bg);
    let box_w = 120u16.min(full.width.saturating_sub(4)).max(40);
    let box_x = full.x + (full.width.saturating_sub(box_w)) / 2;
    let wrap_w = full.width.saturating_sub(8).max(8) as usize;
    let ch = (wrap_plain(&app.composer.text, wrap_w).len().clamp(1, 8) as u16) + 2;
    let composer_y = full.height.saturating_sub(3 + ch);
    let box_y = 4u16.min(composer_y.saturating_sub(8));
    let box_h = composer_y
        .saturating_add(1)
        .saturating_sub(box_y)
        .saturating_add(1)
        .max(10);
    let area = Rect {
        x: box_x,
        y: box_y,
        width: box_w,
        height: box_h,
    };
    frame.render_widget(Clear, area);

    let inner_w = box_w.saturating_sub(2) as usize;
    let list = app.picker_sessions();
    let mut body: Vec<(Line, Option<usize>)> = Vec::new();
    let mut last_group: Option<String> = None;
    if list.is_empty() {
        body.push((Line::from(Span::styled("  no sessions", th.mute())), None));
    }
    for (i, s) in list.iter().enumerate() {
        let g = Session::group_label(&s.cwd);
        if last_group.as_deref() != Some(g.as_str()) {
            if last_group.is_some() {
                body.push((Line::from(""), None));
            }
            body.push((group_row(&g, inner_w, th), None));
            last_group = Some(g);
        }
        body.push((
            session_row(
                s.title.as_str(),
                s.updated,
                i == picker.selected,
                inner_w,
                th,
            ),
            Some(i),
        ));
    }

    let inner_rows = box_h.saturating_sub(2) as usize;
    let list_h = inner_rows.saturating_sub(4).max(1);
    let sel_line = body
        .iter()
        .position(|(_, idx)| *idx == Some(picker.selected))
        .unwrap_or(0);
    let start = (sel_line + 1).saturating_sub(list_h);
    let end = (start + list_h).min(body.len());
    let visible = if start < end { &body[start..end] } else { &[] };

    let mut lines: Vec<Line> = vec![
        overlay_top(box_w as usize, "Resume session", th),
        sided(Line::from(""), inner_w, th, border, false),
        resume_search(
            &picker.query,
            picker.searching,
            if picker.filter_cwd { "Local" } else { "All" },
            inner_w,
            th,
            border,
        ),
        sided(
            Line::from(Span::styled("─".repeat(inner_w), border)),
            inner_w,
            th,
            border,
            false,
        ),
    ];

    let mut hits: Vec<(Rect, usize)> = Vec::new();
    let list_y = box_y + 4;
    for (row, (line, idx)) in visible.iter().enumerate() {
        lines.push(sided(
            line.clone(),
            inner_w,
            th,
            border,
            idx.is_some_and(|i| i == picker.selected),
        ));
        if let Some(i) = idx {
            hits.push((
                Rect {
                    x: box_x + 1,
                    y: list_y + row as u16,
                    width: box_w.saturating_sub(2),
                    height: 1,
                },
                *i,
            ));
        }
    }
    while lines.len() + 2 < box_h as usize {
        lines.push(sided(Line::from(""), inner_w, th, border, false));
    }
    let keys: &[(&str, &str)] = if picker.confirm_delete {
        &[("y", "confirm"), ("n", "cancel")]
    } else {
        &[
            ("↑↓", "nav"),
            ("e", "expand"),
            ("/", "search"),
            ("f", "filter"),
            ("d", "delete"),
        ]
    };
    lines.push(overlay_keys(inner_w, keys, th, border));
    lines.push(overlay_bottom(inner_w, border));

    frame.render_widget(Paragraph::new(lines).style(th.base()), area);

    app.pick_hits.set(hits);
    app.close_hit.set(Some(Rect {
        x: box_x + box_w.saturating_sub(6),
        y: box_y,
        width: 3,
        height: 1,
    }));
}

fn resume_search(
    query: &str,
    searching: bool,
    chip: &str,
    inner_w: usize,
    th: Theme,
    border: Style,
) -> Line<'static> {
    let left = if searching || !query.is_empty() {
        format!("   / {query}_")
    } else {
        "   / to search".into()
    };
    let right_w = chip.width() + 6; // "chip f    "
    let gap = inner_w.saturating_sub(left.width() + right_w);
    Line::from(vec![
        Span::styled("│", border),
        Span::styled(left, th.mute()),
        Span::styled(" ".repeat(gap), th.base()),
        Span::styled(format!("{chip} "), th.mute()),
        Span::styled("f", th.mute()),
        Span::styled("    ", th.base()),
        Span::styled("│", border),
    ])
}

fn group_row(label: &str, inner_w: usize, th: Theme) -> Line<'static> {
    let left = format!("   {label} ");
    let fill = inner_w.saturating_sub(left.width() + 2);
    Line::from(vec![
        Span::styled(left, th.dim()),
        Span::styled("─".repeat(fill), th.mute()),
        Span::styled("  ", th.base()),
    ])
}

fn sided(
    content: Line<'static>,
    inner_w: usize,
    th: Theme,
    border: Style,
    selected: bool,
) -> Line<'static> {
    let pad_style = if selected { th.selected() } else { th.base() };
    let w = crate::text::spans_width(&content.spans);
    let mut spans = vec![Span::styled("│", border)];
    spans.extend(content.spans);
    if w < inner_w {
        spans.push(Span::styled(" ".repeat(inner_w - w), pad_style));
    }
    spans.push(Span::styled("│", border));
    Line::from(spans)
}

fn session_row(
    title: &str,
    updated: chrono::DateTime<chrono::Local>,
    selected: bool,
    inner_w: usize,
    th: Theme,
) -> Line<'static> {
    let time = Session::relative_time(updated);
    let inset = "  ";
    let chev = "› ";
    let hl_w = inner_w.saturating_sub(4);
    let hl_prefix = format!("  {chev}");
    let time_s = format!("{time} ");
    let title_w = hl_w
        .saturating_sub(hl_prefix.width() + time_s.width())
        .max(1);
    let title = truncate_width(title, title_w);
    let gap = hl_w.saturating_sub(hl_prefix.width() + title.width() + time_s.width());
    let sel_dim = Style::default().fg(th.fg_dim).bg(th.bg_sel);
    let title_style = th.base().add_modifier(Modifier::BOLD);
    if selected {
        Line::from(vec![
            Span::styled(inset.to_string(), th.base()),
            Span::styled("  ".to_string(), th.selected()),
            Span::styled(chev, sel_dim),
            Span::styled(
                format!("{title}{}", " ".repeat(gap)),
                th.selected().add_modifier(Modifier::BOLD),
            ),
            Span::styled(time_s, sel_dim),
            Span::styled(inset.to_string(), th.base()),
        ])
    } else {
        Line::from(vec![
            Span::styled(format!("{inset}  "), th.base()),
            Span::styled(chev, th.dim()),
            Span::styled(title, title_style),
            Span::styled(" ".repeat(gap), th.base()),
            Span::styled(time_s, th.dim()),
            Span::styled(inset.to_string(), th.base()),
        ])
    }
}

fn models(frame: &mut Frame, app: &App, th: Theme, items: &[String], selected: usize) {
    let body: Vec<(Line, bool)> = items
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if i == selected {
                (
                    Line::from(Span::styled(format!("  ❯ {m}"), th.selected())),
                    true,
                )
            } else {
                (
                    Line::from(Span::styled(format!("    {m}"), th.base())),
                    false,
                )
            }
        })
        .collect();
    paint_card(
        frame,
        app,
        th,
        "model",
        56,
        body,
        &[("↑↓", "nav"), ("enter", "select"), ("esc", "close")],
    );
}

#[allow(clippy::too_many_arguments)]
fn question(
    frame: &mut Frame,
    app: &App,
    th: Theme,
    prompt: &str,
    hint: &str,
    options: &[String],
    selected: usize,
    draft: &str,
) {
    let mut body = vec![
        (
            Line::from(Span::styled(format!("  {prompt}"), th.accent_bold())),
            false,
        ),
        (
            Line::from(Span::styled(
                format!("  {}", truncate_width(hint, 62)),
                th.dim(),
            )),
            false,
        ),
        (Line::from(""), false),
    ];
    if options.is_empty() {
        body.push((
            Line::from(Span::styled(format!("  ❯ {draft}▏"), th.selected())),
            true,
        ));
        paint_card(
            frame,
            app,
            th,
            "question",
            72,
            body,
            &[("enter", "submit"), ("esc", "cancel")],
        );
        return;
    }
    for (i, opt) in options.iter().enumerate() {
        let st = if i == selected {
            th.selected()
        } else {
            th.base()
        };
        body.push((
            Line::from(Span::styled(format!("  {}. {opt}", i + 1), st)),
            i == selected,
        ));
    }
    paint_card(
        frame,
        app,
        th,
        "question",
        72,
        body,
        &[("↑↓", "nav"), ("enter", "ok"), ("esc", "cancel")],
    );
}
