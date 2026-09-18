use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

pub fn grapheme_len(s: &str) -> usize {
    s.graphemes(true).count()
}

pub fn grapheme_prefix(s: &str, n: usize) -> &str {
    match s.graphemes(true).take(n).map(|g| g.len()).sum::<usize>() {
        0 => "",
        k => &s[..k.min(s.len())],
    }
}

pub fn byte_at_grapheme(s: &str, n: usize) -> usize {
    s.graphemes(true).take(n).map(|g| g.len()).sum()
}

pub fn grapheme_at_byte(s: &str, byte: usize) -> usize {
    s[..byte.min(s.len())].graphemes(true).count()
}

pub fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    for raw in text.split('\n') {
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        let mut w = 0usize;
        for g in raw.graphemes(true) {
            let gw = g.width().max(1);
            if w + gw > width && !line.is_empty() {
                out.push(std::mem::take(&mut line));
                w = 0;
            }
            line.push_str(g);
            w += gw;
        }
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

pub fn wrap_spans<'a>(spans: Vec<Span<'a>>, width: usize, base: Style) -> Vec<Line<'a>> {
    if width == 0 {
        return vec![Line::from("")];
    }
    let mut lines = Vec::new();
    let mut cur: Vec<Span<'a>> = Vec::new();
    let mut w = 0usize;
    for span in spans {
        let style = span.style;
        let content = span.content;
        for g in content
            .into_owned()
            .graphemes(true)
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
        {
            let gw = g.width().max(1);
            if g == "\n" {
                lines.push(Line::from(std::mem::take(&mut cur)).style(base));
                w = 0;
                continue;
            }
            if w + gw > width && !cur.is_empty() {
                lines.push(Line::from(std::mem::take(&mut cur)).style(base));
                w = 0;
            }
            cur.push(Span::styled(g, style));
            w += gw;
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(Line::from(cur).style(base));
    }
    lines
}

pub fn markdown_lines(md: &str, width: usize, th: &Theme) -> Vec<Line<'static>> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(md, opts);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut style_stack = vec![th.base()];
    let mut list_depth = 0u8;
    let mut in_code = false;
    let mut code_buf = String::new();
    let mut code_lang = String::new();
    let mut heading = 0u8;
    let mut pending_break = false;

    let flush_cur = |cur: &mut Vec<Span<'static>>,
                     lines: &mut Vec<Line<'static>>,
                     width: usize,
                     base: Style| {
        if cur.is_empty() {
            lines.push(Line::from("").style(base));
            return;
        }
        let spans = std::mem::take(cur);
        lines.extend(wrap_spans(spans, width, base));
    };

    for ev in parser {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = level as u8;
                style_stack.push(th.accent_bold());
            }
            Event::End(TagEnd::Heading(_)) => {
                flush_cur(&mut cur, &mut lines, width, th.base());
                style_stack.pop();
                heading = 0;
                pending_break = true;
            }
            Event::Start(Tag::Paragraph) => {
                if pending_break {
                    lines.push(Line::from("").style(th.base()));
                    pending_break = false;
                }
            }
            Event::End(TagEnd::Paragraph) => {
                flush_cur(&mut cur, &mut lines, width, th.base());
                pending_break = true;
            }
            Event::Start(Tag::List(_)) => {
                list_depth = list_depth.saturating_add(1);
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
                pending_break = true;
            }
            Event::Start(Tag::Item) => {
                let indent = "  ".repeat(list_depth.saturating_sub(1) as usize);
                cur.push(Span::styled(format!("{indent}• "), th.accent()));
            }
            Event::End(TagEnd::Item) => {
                flush_cur(&mut cur, &mut lines, width, th.base());
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code = true;
                code_buf.clear();
                code_lang = match kind {
                    CodeBlockKind::Fenced(l) => l.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                let label = if code_lang.is_empty() {
                    "code".to_string()
                } else {
                    code_lang.clone()
                };
                lines.push(Line::from(vec![Span::styled(
                    format!("  {label}"),
                    th.mute(),
                )]));
                let inner_w = width.saturating_sub(2).max(8);
                for row in wrap_plain(code_buf.trim_end(), inner_w) {
                    lines.push(Line::from(vec![
                        Span::styled("  ", th.code()),
                        Span::styled(row, th.code().fg(th.fg_dim)),
                    ]));
                }
                code_buf.clear();
                pending_break = true;
            }
            Event::Start(Tag::Emphasis) => {
                let s = style_stack
                    .last()
                    .copied()
                    .unwrap_or_else(|| th.base())
                    .add_modifier(Modifier::ITALIC);
                style_stack.push(s);
            }
            Event::Start(Tag::Strong) => {
                let s = style_stack
                    .last()
                    .copied()
                    .unwrap_or_else(|| th.base())
                    .add_modifier(Modifier::BOLD);
                style_stack.push(s);
            }
            Event::Start(Tag::BlockQuote(_)) => {
                style_stack.push(th.dim().add_modifier(Modifier::ITALIC));
            }
            Event::End(TagEnd::Emphasis | TagEnd::Strong | TagEnd::BlockQuote(_)) => {
                style_stack.pop();
            }
            Event::Text(t) => {
                if in_code {
                    code_buf.push_str(&t);
                } else {
                    let st = if heading > 0 {
                        th.accent_bold()
                    } else {
                        style_stack.last().copied().unwrap_or_else(|| th.base())
                    };
                    cur.push(Span::styled(t.to_string(), st));
                }
            }
            Event::Code(t) => {
                cur.push(Span::styled(
                    format!(" {t} "),
                    Style::default().fg(th.keyword).bg(th.bg_dark),
                ));
            }
            Event::SoftBreak => cur.push(Span::raw(" ")),
            Event::HardBreak => cur.push(Span::raw("\n")),
            Event::Rule => {
                lines.push(Line::from(Span::styled(
                    "─".repeat(width.min(48)),
                    th.mute(),
                )));
            }
            Event::TaskListMarker(done) => {
                cur.push(Span::styled(
                    if done { "[x] " } else { "[ ] " },
                    if done { th.success() } else { th.dim() },
                ));
            }
            _ => {}
        }
    }
    if !cur.is_empty() {
        flush_cur(&mut cur, &mut lines, width, th.base());
    }
    if lines.is_empty() {
        lines.push(Line::from("").style(th.base()));
    }
    lines
}

/// CSI parameter / intermediate bytes (`ESC [ < 64 ; 149 ; 19 M`).
pub(crate) fn csi_param(c: char) -> bool {
    c.is_ascii_digit()
        || matches!(
            c,
            ';' | ':' | '<' | '>' | '?' | '=' | '+' | '-' | '.' | '$' | '"' | '\'' | ' '
        )
}

pub(crate) fn csi_final(c: char) -> bool {
    ('\u{40}'..='\u{7e}').contains(&c)
}

/// Drop terminal escape sequences and stray control bytes from text that
/// arrives outside the key path (bracketed paste, mouse residue). `\n` and
/// `\t` survive.
pub fn sanitize_input(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\x1b' => match it.next() {
                Some('[') => {
                    for c2 in it.by_ref() {
                        if csi_final(c2) {
                            break;
                        }
                    }
                }
                // OSC: ends at BEL or ST.
                Some(']') => {
                    while let Some(c2) = it.next() {
                        if c2 == '\x07' {
                            break;
                        }
                        if c2 == '\x1b' {
                            if it.peek() == Some(&'\\') {
                                it.next();
                            }
                            break;
                        }
                    }
                }
                // SS3: one introducer plus one final byte.
                Some('O') => {
                    it.next();
                }
                _ => {}
            },
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

pub fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}

pub fn truncate_width(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for g in s.graphemes(true) {
        let gw = g.width().max(1);
        if w + gw + 1 > width {
            break;
        }
        out.push_str(g);
        w += gw;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_keeps_thai_graphemes() {
        let rows = wrap_plain("สวัสดี", 2);
        assert!(rows
            .iter()
            .all(|r| r.width() <= 2 || r.chars().count() == 1));
    }

    #[test]
    fn sanitize_drops_escape_residue() {
        assert_eq!(sanitize_input("\x1b[<64;149;19Mhello"), "hello");
        assert_eq!(sanitize_input("a\x1b[27ubb"), "abb");
        assert_eq!(sanitize_input("bell\x07here"), "bellhere");
        assert_eq!(sanitize_input("keep\nme\ttoo"), "keep\nme\ttoo");
    }

    #[test]
    fn markdown_renders_heading_and_code() {
        let th = Theme::night();
        let lines = markdown_lines(
            "# Hi\n\nUse `cargo`:\n\n```rs\nfn main() {}\n```\n",
            40,
            &th,
        );
        let joined: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("Hi"), "{joined}");
        assert!(joined.contains("cargo"), "{joined}");
        assert!(joined.contains("fn main"), "{joined}");
    }
}
