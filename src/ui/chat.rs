use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, Focus, LayoutCache};
use crate::session::{Block as SBlock, ToolStatus};
use crate::text::{markdown_lines, truncate_width, wrap_plain};

pub(super) fn draw_chat(frame: &mut Frame, app: &App, area: Rect, gap: Rect) {
    let th = app.theme;
    let body = if app.show_todos {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(30), Constraint::Length(32)])
            .split(area);
        draw_todos(frame, app, cols[1]);
        cols[0]
    } else {
        area
    };
    let content = Rect {
        x: body.x,
        y: body.y,
        width: body.width.saturating_sub(1),
        height: body.height,
    };
    let rows = render_blocks(app, content.width.saturating_sub(2).max(16) as usize);
    let h = content.height as usize;
    let total = rows.len();
    let max_scroll = total.saturating_sub(h) as u16;
    let scroll = (app.scroll as usize).min(max_scroll as usize);
    let start = total.saturating_sub(h + scroll);
    let end = (start + h).min(total);
    let view: Vec<Line> = if start < end {
        rows[start..end].to_vec()
    } else {
        vec![]
    };
    frame.render_widget(Paragraph::new(view).style(th.base()), content);

    let mut arrow_down = None;
    if max_scroll > 0 {
        let thumb_h = (((h.max(1) as u32 * body.height as u32) / total.max(1) as u32) as u16)
            .max(1)
            .min(body.height);
        let room = body.height.saturating_sub(thumb_h);
        let thumb_off = if max_scroll == 0 {
            room
        } else {
            room.saturating_sub(((app.scroll as u32 * room as u32) / max_scroll as u32) as u16)
        };
        let bar_x = body.x + body.width.saturating_sub(1);
        for y in thumb_off..thumb_off.saturating_add(thumb_h).min(body.height) {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "█",
                    Style::default().fg(th.bg_light).bg(th.bg_light),
                )),
                Rect {
                    x: bar_x,
                    y: body.y + y,
                    width: 1,
                    height: 1,
                },
            );
        }
    }
    if app.scroll > 0 {
        let r = Rect {
            x: gap.x + gap.width / 2,
            y: gap.y,
            width: 1,
            height: 1,
        };
        frame.render_widget(Paragraph::new(Span::styled("▼", th.dim())), r);
        arrow_down = Some(r);
    }
    app.layout.set(LayoutCache {
        body,
        max_scroll,
        arrow_down,
    });
}

fn render_blocks(app: &App, width: usize) -> Vec<Line<'static>> {
    let th = app.theme;
    let mut rows = Vec::new();
    if app.session.blocks.is_empty() {
        rows.push(Line::from(""));
        rows.push(Line::from(Span::styled(
            "  Waiting on a first prompt.",
            th.mute(),
        )));
        return rows;
    }
    for (i, block) in app.session.blocks.iter().enumerate() {
        let sel = app.focus == Focus::Scrollback && i == app.selected_block;
        match block {
            SBlock::User { text, time } => {
                rows.push(Line::from(""));
                let style = if sel { th.user_band() } else { th.base() };
                let mut first = true;
                let tpad = if time.is_empty() { 0 } else { time.len() + 1 };
                for line in wrap_plain(text, width.saturating_sub(8 + tpad)) {
                    let (prefix, pstyle) = if first {
                        ("    ❯ ", Style::default().fg(th.fg_bright).bg(th.bg_light))
                    } else {
                        ("      ", style)
                    };
                    let mut spans = vec![
                        Span::styled(prefix, pstyle),
                        Span::styled(line.clone(), style),
                    ];
                    if first && !time.is_empty() {
                        let used = unicode_width::UnicodeWidthStr::width(prefix)
                            + unicode_width::UnicodeWidthStr::width(line.as_str());
                        let gap = width.saturating_sub(
                            used + unicode_width::UnicodeWidthStr::width(time.as_str()),
                        );
                        spans.push(Span::styled(" ".repeat(gap), style));
                        spans.push(Span::styled(time.clone(), th.dim()));
                    }
                    first = false;
                    rows.push(Line::from(spans));
                }
            }
            SBlock::Assistant { text, time } => {
                rows.push(Line::from(""));
                let mut md = markdown_lines(text, width.saturating_sub(6), &th);
                for (i, mut line) in md.drain(..).enumerate() {
                    let mut spans = vec![Span::styled("    ", th.base())];
                    spans.append(&mut line.spans);
                    if i == 0 && !time.is_empty() {
                        let used: usize = spans
                            .iter()
                            .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
                            .sum();
                        let gap = width.saturating_sub(
                            used + unicode_width::UnicodeWidthStr::width(time.as_str()),
                        );
                        spans.push(Span::styled(" ".repeat(gap), th.base()));
                        spans.push(Span::styled(time.clone(), th.dim()));
                    }
                    rows.push(Line::from(spans));
                }
            }
            SBlock::Thinking { text, folded, ms } => {
                rows.push(Line::from(""));
                let secs = if *ms > 0 {
                    format!("Thought for {:.1}s", *ms as f64 / 1000.0)
                } else {
                    "Thought".into()
                };
                rows.push(Line::from(Span::styled(format!("    ◆ {secs}"), th.dim())));
                if !*folded {
                    for line in wrap_plain(text, width.saturating_sub(8)) {
                        rows.push(Line::from(Span::styled(
                            format!("      {line}"),
                            th.dim().add_modifier(Modifier::ITALIC),
                        )));
                    }
                }
            }
            SBlock::Tool {
                name,
                detail,
                output,
                status,
                folded,
                ..
            } => {
                rows.push(Line::from(""));
                let (glyph, st) = match status {
                    ToolStatus::Running => (app.spinner().to_string(), th.warn()),
                    ToolStatus::Ok => ("✓".into(), th.success()),
                    ToolStatus::Failed => ("✗".into(), th.error()),
                    ToolStatus::Denied => ("⊘".into(), th.mute()),
                };
                let label = match name.as_str() {
                    "bash" => format!("Run {}", truncate_width(detail, width.saturating_sub(12))),
                    "read_file" => {
                        format!("Read {}", truncate_width(detail, width.saturating_sub(12)))
                    }
                    other => format!(
                        "{other} {}",
                        truncate_width(detail, width.saturating_sub(other.len() + 8))
                    ),
                };
                let bullet = if *status == ToolStatus::Running {
                    glyph
                } else if name == "read_file" {
                    "◈".into()
                } else {
                    "◆".into()
                };
                rows.push(Line::from(Span::styled(
                    format!("    {bullet} {label}"),
                    st,
                )));
                if !*folded && !output.is_empty() {
                    let inner = width.saturating_sub(6).max(12);
                    let mut lines: Vec<String> = wrap_plain(output.trim_end(), inner);
                    if lines.len() > 18 {
                        let extra = lines.len() - 16;
                        lines.truncate(16);
                        lines.push(format!("… {extra} more lines"));
                    }
                    let edge = Style::default().fg(th.border).bg(th.bg);
                    let body = th.dim();
                    rows.push(Line::from(Span::styled(
                        format!("    ╭{}╮", "─".repeat(inner + 2)),
                        edge,
                    )));
                    for l in lines {
                        let pad =
                            inner.saturating_sub(unicode_width::UnicodeWidthStr::width(l.as_str()));
                        rows.push(Line::from(vec![
                            Span::styled("    │ ", edge),
                            Span::styled(l, body),
                            Span::raw(" ".repeat(pad)),
                            Span::styled(" │", edge),
                        ]));
                    }
                    rows.push(Line::from(Span::styled(
                        format!("    ╰{}╯", "─".repeat(inner + 2)),
                        edge,
                    )));
                }
            }
            SBlock::Notice { text } => {
                rows.push(Line::from(""));
                for line in wrap_plain(text, width.saturating_sub(6)) {
                    rows.push(Line::from(Span::styled(format!("    {line}"), th.dim())));
                }
            }
            SBlock::Error { text } => {
                rows.push(Line::from(""));
                for line in wrap_plain(text, width.saturating_sub(6)) {
                    rows.push(Line::from(Span::styled(format!("    {line}"), th.error())));
                }
            }
        }
    }
    if app.running {
        rows.push(Line::from(""));
        rows.push(Line::from(Span::styled(
            format!("    {} working", app.spinner()),
            th.dim(),
        )));
    }
    rows.push(Line::from(""));
    rows
}

fn draw_todos(frame: &mut Frame, app: &App, area: Rect) {
    let th = app.theme;
    let block = Block::default()
        .title(" todos ")
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(if app.focus == Focus::Todos {
            th.gold
        } else {
            th.border
        }))
        .style(th.base());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let plan = std::fs::read_to_string(app.session.plan_path()).ok();
    let mut lines = Vec::new();
    if app.session.todos.is_empty() && plan.is_none() {
        lines.push(Line::from(Span::styled("  no tasks yet", th.mute())));
    }
    for t in &app.session.todos {
        let mark = match t.status.as_str() {
            "completed" => Span::styled(" ✓ ", th.success()),
            "in_progress" => Span::styled(" ▸ ", th.accent()),
            _ => Span::styled(" ○ ", th.mute()),
        };
        lines.push(Line::from(vec![
            mark,
            Span::styled(
                truncate_width(&t.content, inner.width.saturating_sub(4) as usize),
                th.base(),
            ),
        ]));
    }
    if let Some(p) = plan {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(" plan", th.mute())));
        for row in p
            .lines()
            .take(inner.height.saturating_sub(lines.len() as u16 + 1) as usize)
        {
            lines.push(Line::from(Span::styled(
                truncate_width(row, inner.width as usize),
                th.dim(),
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}
