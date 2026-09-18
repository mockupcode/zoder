use std::path::Path;
use std::time::{Duration, Instant};

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Focus, LayoutCache};
use crate::session::{Block as SBlock, ToolStatus};
use crate::text::{markdown_lines, truncate_width, wrap_plain};
use crate::theme::Theme;

/// Rendered transcript rows, kept per block. Only blocks whose fingerprint
/// changed are rebuilt; only the visible slice is copied each frame.
#[derive(Default)]
pub struct ChatCache {
    width: usize,
    keys: Vec<u64>,
    blocks: Vec<Vec<Line<'static>>>,
}

impl ChatCache {
    fn set_width(&mut self, width: usize) {
        if self.width == width {
            return;
        }
        self.width = width;
        self.keys.clear();
        self.blocks.clear();
    }

    fn put(&mut self, i: usize, key: u64, lines: Vec<Line<'static>>) {
        if i < self.blocks.len() {
            self.blocks[i] = lines;
            self.keys[i] = key;
        } else {
            self.blocks.push(lines);
            self.keys.push(key);
        }
    }

    fn row_count(&self) -> usize {
        self.blocks.iter().map(|b| b.len()).sum()
    }
}

/// `plan.md` feeds the todos pane; throttle the read so drawing stays cheap.
#[derive(Default)]
pub struct PlanCache {
    checked: Option<Instant>,
    text: Option<String>,
}

const PLAN_TTL: Duration = Duration::from_millis(500);
const PLACEHOLDER_KEY: u64 = 0xd0de_0bad_cafe_0001;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

impl PlanCache {
    pub fn get(&mut self, path: &Path) -> Option<String> {
        let fresh = self.checked.is_some_and(|t| t.elapsed() < PLAN_TTL);
        if !fresh {
            self.checked = Some(Instant::now());
            self.text = std::fs::read_to_string(path).ok();
        }
        self.text.clone()
    }
}

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
    let width = content.width.saturating_sub(2).max(16) as usize;
    let total = transcript(app, width);
    let h = content.height as usize;
    let max_scroll = total.saturating_sub(h) as u16;
    keep_anchor(app, body, max_scroll);
    let scroll = (app.scroll.get() as usize).min(max_scroll as usize);
    let start = total.saturating_sub(h + scroll);
    let end = (start + h).min(total);
    frame.render_widget(
        Paragraph::new(window(app, start, end)).style(th.base()),
        content,
    );

    if max_scroll > 0 {
        draw_scrollbar(frame, body, h, total, app.scroll.get(), max_scroll, th);
    }
    let arrow_down = if app.scroll.get() > 0 {
        let r = Rect {
            x: gap.x + gap.width / 2,
            y: gap.y,
            width: 1,
            height: 1,
        };
        frame.render_widget(Paragraph::new(Span::styled("▼", th.dim())), r);
        Some(r)
    } else {
        None
    };
    app.layout.set(LayoutCache {
        body,
        max_scroll,
        arrow_down,
    });
}

/// `scroll` counts back from the bottom; bump it when rows are appended so the
/// visible window does not slide under the reader.
fn keep_anchor(app: &App, body: Rect, max_scroll: u16) {
    let prev = app.layout.get();
    if prev.body != body || app.follow {
        return;
    }
    let shift = max_scroll as i32 - prev.max_scroll as i32;
    if shift != 0 {
        let next = (app.scroll.get() as i32 + shift).clamp(0, max_scroll as i32);
        app.scroll.set(next as u16);
    }
}

fn draw_scrollbar(
    frame: &mut Frame,
    body: Rect,
    h: usize,
    total: usize,
    scroll: u16,
    max_scroll: u16,
    th: Theme,
) {
    let thumb_h = (((h.max(1) as u32 * body.height as u32) / total.max(1) as u32) as u16)
        .max(1)
        .min(body.height);
    let room = body.height.saturating_sub(thumb_h);
    let thumb_off = room.saturating_sub(((scroll as u32 * room as u32) / max_scroll as u32) as u16);
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

struct Fingerprint(u64);

impl Fingerprint {
    fn new() -> Self {
        Self(FNV_OFFSET)
    }

    fn mix(mut self, v: u64) -> Self {
        self.0 ^= v;
        self.0 = self.0.wrapping_mul(FNV_PRIME);
        self
    }

    fn str(mut self, s: &str) -> Self {
        let b = s.as_bytes();
        self = self.mix(b.len() as u64);
        for &x in b.iter().take(16) {
            self = self.mix(x as u64);
        }
        for &x in b.iter().skip(b.len().saturating_sub(16)) {
            self = self.mix(x as u64);
        }
        if b.len() > 32 {
            for &x in b.iter().skip(b.len() / 2).take(8) {
                self = self.mix(x as u64);
            }
        }
        self
    }

    fn finish(self) -> u64 {
        self.0
    }
}

fn block_key(app: &App, i: usize, width: usize) -> u64 {
    let sel = (app.focus == Focus::Scrollback && i == app.selected_block) as u64;
    let mut fp = Fingerprint::new().mix((width as u64) ^ (sel << 33));
    fp = match &app.session.blocks[i] {
        SBlock::User { text, time } => fp.mix(1).str(text).str(time),
        SBlock::Assistant { text, time } => fp.mix(2).str(text).str(time),
        SBlock::Thinking { text, folded, ms } => fp.mix(3).str(text).mix(*folded as u64).mix(*ms),
        SBlock::Tool {
            name,
            detail,
            output,
            status,
            folded,
            ..
        } => fp
            .mix(4)
            .str(name)
            .str(detail)
            .str(output)
            .mix(*folded as u64)
            .mix(match status {
                ToolStatus::Running => app.tick ^ 0x5555,
                ToolStatus::Ok => 5,
                ToolStatus::Failed => 6,
                ToolStatus::Denied => 7,
            }),
        SBlock::Notice { text } => fp.mix(8).str(text),
        SBlock::Error { text } => fp.mix(9).str(text),
    };
    fp.finish()
}

fn transcript(app: &App, width: usize) -> usize {
    let mut cache = app.chat_cache.borrow_mut();
    cache.set_width(width);
    if app.session.blocks.is_empty() {
        if cache.keys.as_slice() != [PLACEHOLDER_KEY] {
            cache.keys = vec![PLACEHOLDER_KEY];
            cache.blocks = vec![vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Waiting on a first prompt.",
                    app.theme.mute(),
                )),
            ]];
        }
        return cache.blocks[0].len() + tail_rows(app);
    }
    for (i, block) in app.session.blocks.iter().enumerate() {
        let key = block_key(app, i, width);
        if cache.keys.get(i) == Some(&key) {
            continue;
        }
        cache.put(i, key, render_block(app, block, i, width));
    }
    let n = app.session.blocks.len();
    cache.keys.truncate(n);
    cache.blocks.truncate(n);
    cache.row_count() + tail_rows(app)
}

fn tail_rows(app: &App) -> usize {
    if app.running {
        2
    } else {
        1
    }
}

fn window(app: &App, start: usize, end: usize) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(end.saturating_sub(start));
    if start >= end {
        return out;
    }
    let mut pos = 0usize;
    {
        let cache = app.chat_cache.borrow();
        for rows in &cache.blocks {
            push_window(rows, start, end, &mut pos, &mut out);
        }
    }
    let th = app.theme;
    let tail: Vec<Line<'static>> = if app.running {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("    {} working", app.spinner()),
                th.dim(),
            )),
        ]
    } else {
        vec![Line::from("")]
    };
    for row in &tail {
        push_window(std::slice::from_ref(row), start, end, &mut pos, &mut out);
    }
    out
}

fn push_window(
    rows: &[Line<'static>],
    start: usize,
    end: usize,
    pos: &mut usize,
    out: &mut Vec<Line<'static>>,
) {
    if *pos < end && *pos + rows.len() > start {
        let from = start.saturating_sub(*pos);
        let to = (end - *pos).min(rows.len());
        out.extend(rows[from..to].iter().cloned());
    }
    *pos += rows.len();
}

fn render_block(app: &App, block: &SBlock, i: usize, width: usize) -> Vec<Line<'static>> {
    let th = app.theme;
    let sel = app.focus == Focus::Scrollback && i == app.selected_block;
    match block {
        SBlock::User { text, time } => user_lines(text, time, width, sel, th),
        SBlock::Assistant { text, time } => assistant_lines(text, time, width, th),
        SBlock::Thinking { text, folded, ms } => thinking_lines(text, *folded, *ms, width, th),
        SBlock::Tool {
            name,
            detail,
            output,
            status,
            folded,
            ..
        } => tool_lines(app, name, detail, output, *status, *folded, width),
        SBlock::Notice { text } => padded_wrap(text, width, th.dim()),
        SBlock::Error { text } => padded_wrap(text, width, th.error()),
    }
}

fn user_lines(text: &str, time: &str, width: usize, sel: bool, th: Theme) -> Vec<Line<'static>> {
    let mut rows = vec![Line::from("")];
    let style = if sel { th.user_band() } else { th.base() };
    let mut first = true;
    let tpad = if time.is_empty() { 0 } else { time.len() + 1 };
    for line in wrap_plain(text, width.saturating_sub(8 + tpad)) {
        let (prefix, pstyle) = if first {
            ("    ❯ ", Style::default().fg(th.fg_bright).bg(th.bg_light))
        } else {
            ("      ", style)
        };
        let mut spans = vec![Span::styled(prefix, pstyle), Span::styled(line, style)];
        if first {
            append_time(&mut spans, time, width, style, th.dim());
        }
        first = false;
        rows.push(Line::from(spans));
    }
    rows
}

fn assistant_lines(text: &str, time: &str, width: usize, th: Theme) -> Vec<Line<'static>> {
    let mut rows = vec![Line::from("")];
    let mut md = markdown_lines(text, width.saturating_sub(6), &th);
    for (i, mut line) in md.drain(..).enumerate() {
        let mut spans = vec![Span::styled("    ", th.base())];
        spans.append(&mut line.spans);
        if i == 0 {
            append_time(&mut spans, time, width, th.base(), th.dim());
        }
        rows.push(Line::from(spans));
    }
    rows
}

fn thinking_lines(
    text: &str,
    folded: bool,
    ms: u64,
    width: usize,
    th: Theme,
) -> Vec<Line<'static>> {
    let mut rows = vec![Line::from("")];
    let secs = if ms > 0 {
        format!("Thought for {:.1}s", ms as f64 / 1000.0)
    } else {
        "Thought".into()
    };
    rows.push(Line::from(Span::styled(format!("    ◆ {secs}"), th.dim())));
    if !folded {
        for line in wrap_plain(text, width.saturating_sub(8)) {
            rows.push(Line::from(Span::styled(
                format!("      {line}"),
                th.dim().add_modifier(Modifier::ITALIC),
            )));
        }
    }
    rows
}

fn tool_lines(
    app: &App,
    name: &str,
    detail: &str,
    output: &str,
    status: ToolStatus,
    folded: bool,
    width: usize,
) -> Vec<Line<'static>> {
    let th = app.theme;
    let mut rows = vec![Line::from("")];
    let (glyph, st) = match status {
        ToolStatus::Running => (app.spinner().to_string(), th.warn()),
        ToolStatus::Ok => ("✓".into(), th.success()),
        ToolStatus::Failed => ("✗".into(), th.error()),
        ToolStatus::Denied => ("⊘".into(), th.mute()),
    };
    let canon = crate::tools::canonicalize(name);
    let is_edit = matches!(canon, "write" | "edit" | "multiedit" | "lsp_replace_symbol");
    let label = match canon {
        "bash" => format!("Run {}", truncate_width(detail, width.saturating_sub(12))),
        "view" => format!("View {}", truncate_width(detail, width.saturating_sub(12))),
        "write" | "edit" | "multiedit" => {
            format!("Edit {}", truncate_width(detail, width.saturating_sub(12)))
        }
        other => format!(
            "{other} {}",
            truncate_width(detail, width.saturating_sub(other.len() + 8))
        ),
    };
    let bullet = if status == ToolStatus::Running {
        glyph
    } else if canon == "view" {
        "◈".into()
    } else {
        "◆".into()
    };
    let title_st = if is_edit && status == ToolStatus::Ok {
        th.success()
    } else {
        st
    };
    rows.push(Line::from(Span::styled(
        format!("    {bullet} {label}"),
        title_st,
    )));
    if !folded && !output.is_empty() {
        if is_edit {
            if let Some(hunk) = crate::tools::parse_edit_diff(output) {
                rows.extend(diff_rows(&hunk, width, th));
            } else {
                rows.extend(output_box(output, width, th));
            }
        } else {
            rows.extend(output_box(output, width, th));
        }
    }
    rows
}

fn diff_rows(hunk: &[(char, u32, String)], width: usize, th: Theme) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let truncated = hunk.len() > 48;
    let head = if truncated { 24 } else { hunk.len() };
    let tail_from = if truncated {
        hunk.len() - 16
    } else {
        hunk.len()
    };
    for (i, (tag, num, text)) in hunk.iter().enumerate() {
        if truncated && i == head {
            rows.push(Line::from(Span::styled(
                format!("      … {} more lines", hunk.len() - 40),
                th.dim(),
            )));
        }
        if truncated && i >= head && i < tail_from {
            continue;
        }
        let (bg, fg) = match tag {
            '+' => (th.diff_ins_bg, th.fg),
            '-' => (th.diff_del_bg, th.fg),
            _ => (th.bg, th.fg),
        };
        let num_st = Style::default().fg(th.fg_dim).bg(bg);
        let body_st = Style::default().fg(fg).bg(bg);
        let body = truncate_width(text, width.saturating_sub(8));
        let prefix = format!("  {num:>4} ");
        let used = UnicodeWidthStr::width(prefix.as_str()) + UnicodeWidthStr::width(body.as_str());
        let fill = width.saturating_sub(used);
        rows.push(Line::from(vec![
            Span::styled(prefix, num_st),
            Span::styled(body, body_st),
            Span::styled(" ".repeat(fill), Style::default().bg(bg)),
        ]));
    }
    rows
}

fn output_box(output: &str, width: usize, th: Theme) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(6).max(12);
    let mut lines: Vec<String> = wrap_plain(output.trim_end(), inner);
    if lines.len() > 18 {
        let extra = lines.len() - 16;
        lines.truncate(16);
        lines.push(format!("… {extra} more lines"));
    }
    let edge = Style::default().fg(th.border).bg(th.bg);
    let body = th.dim();
    let mut rows = vec![Line::from(Span::styled(
        format!("    ╭{}╮", "─".repeat(inner + 2)),
        edge,
    ))];
    for l in lines {
        let pad = inner.saturating_sub(UnicodeWidthStr::width(l.as_str()));
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
    rows
}

fn padded_wrap(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let mut rows = vec![Line::from("")];
    for line in wrap_plain(text, width.saturating_sub(6)) {
        rows.push(Line::from(Span::styled(format!("    {line}"), style)));
    }
    rows
}

fn append_time(spans: &mut Vec<Span<'static>>, time: &str, width: usize, fill: Style, dim: Style) {
    if time.is_empty() {
        return;
    }
    let used = crate::text::spans_width(spans);
    let gap = width.saturating_sub(used + UnicodeWidthStr::width(time));
    spans.push(Span::styled(" ".repeat(gap), fill));
    spans.push(Span::styled(time.to_string(), dim));
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
    let plan = app.plan_cache.borrow_mut().get(&app.session.plan_path());
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
