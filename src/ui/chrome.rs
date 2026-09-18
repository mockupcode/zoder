use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use super::inset;
use crate::app::{App, Focus, Overlay, Screen};
use crate::text::wrap_plain;

pub(super) fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let th = app.theme;
    let inner = inset(area, 2);
    let cwd = app.session.cwd.display().to_string();
    let home = dirs::home_dir()
        .map(|h| h.display().to_string())
        .unwrap_or_default();
    let short = if !home.is_empty() && cwd.starts_with(&home) {
        format!("~{}", &cwd[home.len()..])
    } else {
        cwd
    };
    let branch = format!("⎇ {} ", app.branch);
    let left = Line::from(vec![
        Span::styled(branch, th.base().add_modifier(Modifier::BOLD)),
        Span::styled(short, th.mute()),
    ]);
    let right = if app.screen == Screen::Chat {
        let used = app
            .session
            .prompt_tokens
            .saturating_add(app.session.eval_tokens);
        if used > 0 {
            format!("{used} tok")
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    frame.render_widget(Paragraph::new(left).style(th.base()), inner);
    if !right.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(right, th.mute()))
                .style(th.base())
                .alignment(Alignment::Right),
            inner,
        );
    }
}

pub(super) fn draw_composer(frame: &mut Frame, app: &App, area: Rect) {
    let th = app.theme;
    let focused = app.focus == Focus::Prompt && matches!(app.overlay, Overlay::None);
    let border = th.prompt_border;
    let _ = focused;
    let stash = if app.composer.stash.is_some() {
        " · stashed"
    } else {
        ""
    };
    let qn = if app.queue.is_empty() {
        String::new()
    } else {
        format!(" · queued {}", app.queue.len())
    };
    let mode_owned;
    let mode: &str = match app.composer.kind {
        crate::composer::DraftKind::Shell => "shell",
        crate::composer::DraftKind::Chat => {
            mode_owned = app.session.mode.label().to_lowercase();
            &mode_owned
        }
    };
    let bottom = if app.running {
        Line::from(Span::styled(
            format!(" {}{stash}{qn} ", app.spinner()),
            Style::default().fg(th.fg_dim).bg(th.bg),
        ))
    } else {
        Line::from(vec![
            Span::styled(
                format!(" {} ", app.session.model),
                Style::default().fg(th.fg_mid).bg(th.bg),
            ),
            Span::styled("·", Style::default().fg(th.fg_mute).bg(th.bg)),
            Span::styled(
                format!(" {mode}{stash}{qn} ",),
                Style::default().fg(th.fg_dim).bg(th.bg),
            ),
        ])
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border).bg(th.bg))
        .title_bottom(bottom.right_aligned())
        .style(th.base());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let prefix = match app.composer.kind {
        crate::composer::DraftKind::Shell => "! ",
        crate::composer::DraftKind::Chat => "❯ ",
    };
    let pad = UnicodeWidthStr::width(prefix) + 1;
    let inner_w = inner.width.saturating_sub(pad as u16).max(1) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for (i, raw) in app.composer.text.split('\n').enumerate() {
        let wrapped = wrap_plain(raw, inner_w);
        for (j, w) in wrapped.iter().enumerate() {
            if i == 0 && j == 0 {
                let slash_hit = app
                    .slash_items()
                    .is_some_and(|c| !c.is_empty())
                    || app.at_entries().is_some_and(|c| !c.is_empty());
                let text_st = if slash_hit {
                    Style::default().fg(Color::Rgb(110, 163, 254)).bg(th.bg)
                } else {
                    th.base()
                };
                lines.push(Line::from(vec![
                    Span::styled(" ", th.base()),
                    Span::styled(prefix, Style::default().fg(th.fg_bright).bg(th.bg)),
                    Span::styled(w.clone(), text_st),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled(" ".repeat(pad), th.base()),
                    Span::styled(w.clone(), th.base()),
                ]));
            }
        }
    }
    frame.render_widget(Paragraph::new(lines).style(th.base()), inner);

    if focused {
        let prefix_w = 1 + UnicodeWidthStr::width(prefix) as u16;
        let (col, row) = app
            .composer
            .visual_cursor_col(inner.width.saturating_sub(prefix_w).max(1) as usize);
        let x = inner.x + col + prefix_w;
        let y = inner.y + row;
        if x < inner.x + inner.width && y < inner.y + inner.height {
            frame.set_cursor_position((x.min(inner.x + inner.width - 1), y));
        }
    }
}

pub(super) fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let th = app.theme;
    let inner = inset(area, 2);
    if let Some((t, _)) = &app.toast {
        frame.render_widget(
            Paragraph::new(Span::styled(
                t.clone(),
                Style::default().fg(th.fg_dim).bg(th.bg),
            ))
            .style(th.base()),
            inner,
        );
        return;
    }
    let key = Style::default().fg(th.fg_bright).bg(th.bg);
    let lab = Style::default().fg(th.fg_dim).bg(th.bg);
    let pipe = Style::default().fg(th.fg_mute).bg(th.bg);
    let mut spans: Vec<Span> = Vec::new();
    let mut hint = |k: &str, l: &str| {
        if !spans.is_empty() {
            spans.push(Span::styled(" | ", pipe));
        }
        spans.push(Span::styled(k.to_string(), key));
        spans.push(Span::styled(format!(":{l}"), lab));
    };
    if app.running {
        hint("Ctrl+c", "cancel");
    }
    if !app.composer.is_empty() {
        if app.composer.multiline {
            hint("Enter", "newline");
            hint("Shift+Enter", "send");
        } else {
            hint("Enter", "send");
            hint("Shift+Enter", "newline");
        }
    }
    hint("Shift+Tab", "mode");
    hint("Ctrl+x", "shortcuts");
    frame.render_widget(Paragraph::new(Line::from(spans)).style(th.base()), inner);
}

fn slash_view_start(sel: usize, n: usize, view: usize) -> usize {
    if n <= view {
        0
    } else {
        let max_start = n - view;
        sel.saturating_sub(view / 2).min(max_start)
    }
}

struct CompRow<'a> {
    name: &'a str,
    about: &'a str,
    selected: bool,
    hits: Vec<usize>,
}

fn draw_comp_panel(
    frame: &mut Frame,
    app: &App,
    composer: Rect,
    rows: &[CompRow<'_>],
    sel: usize,
    sigil: &str,
) {
    if rows.is_empty() {
        return;
    }
    let th = app.theme;
    const VIEW: usize = 7;
    let n = rows.len();
    let view = VIEW.min(n).max(1);
    let start = slash_view_start(sel, n, view);
    let h = view as u16;
    let area = Rect {
        x: composer.x + 2,
        y: composer.y.saturating_sub(h),
        width: composer.width.saturating_sub(4),
        height: h,
    };
    frame.render_widget(Clear, area);
    let panel = Style::default().fg(th.fg).bg(th.bg_light);
    frame.render_widget(Block::default().style(panel), area);
    let bar = n > view;
    let inner_w = area.width.saturating_sub(if bar { 1 } else { 0 }) as usize;
    let name_w = 28;
    let mut pick: Vec<(Rect, usize)> = Vec::new();
    let (thumb_y, thumb_h) = if bar {
        let thb = (view * view / n).max(1);
        let y = start * (view - thb) / (n - view);
        (y, thb)
    } else {
        (0, 0)
    };
    let track = Color::Rgb(28, 28, 28);
    let sigil_w = UnicodeWidthStr::width(sigil);
    let prefix_w = 4 + sigil_w;
    for row in 0..view {
        let i = start + row;
        let item = &rows[i];
        let y = area.y + row as u16;
        pick.push((
            Rect {
                x: area.x,
                y,
                width: inner_w as u16,
                height: 1,
            },
            i,
        ));
        let (row_bg, name_st, about_st, chev_st) = if item.selected {
            (
                th.bg_sel,
                Style::default()
                    .fg(th.fg)
                    .bg(th.bg_sel)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(th.fg_dim).bg(th.bg_sel),
                Style::default().fg(th.fg).bg(th.bg_sel),
            )
        } else {
            (
                th.bg_light,
                Style::default().fg(th.fg).bg(th.bg_light),
                Style::default().fg(th.fg_dim).bg(th.bg_light),
                panel,
            )
        };
        let pad = Style::default().fg(th.fg).bg(row_bg);
        let hit_st = Style::default().fg(Color::Rgb(110, 163, 254)).bg(row_bg);
        let mut name_spans: Vec<Span> = item
            .name
            .chars()
            .enumerate()
            .map(|(ci, ch)| {
                let st = if item.hits.contains(&ci) {
                    if item.selected {
                        hit_st.add_modifier(Modifier::BOLD)
                    } else {
                        hit_st
                    }
                } else {
                    name_st
                };
                Span::styled(ch.to_string(), st)
            })
            .collect();
        let nw = UnicodeWidthStr::width(item.name);
        if nw < name_w {
            name_spans.push(Span::styled(" ".repeat(name_w - nw), pad));
        }
        let about = crate::text::truncate_width(item.about, inner_w.saturating_sub(prefix_w + name_w + 1));
        let gap = inner_w.saturating_sub(
            prefix_w + name_w + 1 + UnicodeWidthStr::width(about.as_str()),
        );
        let mut spans = vec![
            Span::styled("  ", pad),
            Span::styled(if item.selected { "❯" } else { " " }, chev_st),
            Span::styled(" ", pad),
        ];
        if !sigil.is_empty() {
            spans.push(Span::styled(sigil.to_string(), pad));
        }
        spans.extend(name_spans);
        spans.push(Span::styled(" ", about_st));
        spans.push(Span::styled(about, about_st));
        spans.push(Span::styled(" ".repeat(gap), pad));
        if bar {
            let thumb = row >= thumb_y && row < thumb_y + thumb_h;
            spans.push(Span::styled(
                " ",
                Style::default().bg(if thumb { th.fg_mute } else { track }),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(row_bg)),
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
        );
    }
    app.pick_hits.set(pick);
    let count = n.to_string();
    let cw = UnicodeWidthStr::width(count.as_str()) as u16;
    if cw > 0 && area.y > 0 && area.width > cw {
        let cx = area.x + area.width.saturating_sub(cw + if bar { 1 } else { 0 });
        frame.render_widget(
            Paragraph::new(Span::styled(
                count,
                Style::default().fg(th.fg_dim).bg(th.bg),
            )),
            Rect {
                x: cx,
                y: area.y.saturating_sub(1),
                width: cw,
                height: 1,
            },
        );
    }
}

pub(super) fn draw_dropdowns(frame: &mut Frame, app: &App, composer: Rect) {
    if let Some(cmds) = app.slash_items() {
        if cmds.is_empty() {
            return;
        }
        let q = app.composer.slash_query().unwrap_or("");
        let rows: Vec<CompRow> = cmds
            .iter()
            .enumerate()
            .map(|(i, c)| CompRow {
                name: c.name,
                about: c.about,
                selected: i == app.slash_sel,
                hits: crate::slash::match_indices(c.name, q).unwrap_or_default(),
            })
            .collect();
        draw_comp_panel(frame, app, composer, &rows, app.slash_sel, "/");
        return;
    }
    if let Some(files) = app.at_entries() {
        if files.is_empty() {
            return;
        }
        let q = app.composer.at_query().unwrap_or("");
        let filter = q.trim_start_matches('!');
        let filter = match filter.rfind('/') {
            Some(i) => &filter[i + 1..],
            None => filter,
        };
        let rows: Vec<CompRow> = files
            .iter()
            .enumerate()
            .map(|(i, f)| CompRow {
                name: f.label.as_str(),
                about: if f.is_dir { "folder" } else { "" },
                selected: i == app.file_sel,
                hits: crate::slash::match_indices(&f.label, filter).unwrap_or_default(),
            })
            .collect();
        draw_comp_panel(frame, app, composer, &rows, app.file_sel, "");
    }
}
