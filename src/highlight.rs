use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::Color;
use syntect::easy::ScopeRegionIterator;
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};

use crate::theme::Theme;

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_nonewlines)
}

/// Token colors from our theme. Background is applied by the caller (diff bands).
pub fn line_spans(path: &str, line: &str, th: &Theme) -> Vec<(Color, String)> {
    let ss = syntaxes();
    let syntax = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|ext| ss.find_syntax_by_extension(ext))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let mut state = ParseState::new(syntax);
    let ops = match state.parse_line(line, ss) {
        Ok(ops) => ops,
        Err(_) => return vec![(th.fg, line.to_string())],
    };
    let mut stack = ScopeStack::new();
    let mut out = Vec::new();
    for (text, op) in ScopeRegionIterator::new(&ops, line) {
        if stack.apply(op).is_err() {
            if !text.is_empty() {
                out.push((th.fg, text.to_string()));
            }
            continue;
        }
        if text.is_empty() {
            continue;
        }
        out.push((color_for(&stack, th), text.to_string()));
    }
    if out.is_empty() {
        vec![(th.fg, line.to_string())]
    } else {
        out
    }
}

fn color_for(stack: &ScopeStack, th: &Theme) -> Color {
    if has(stack, "comment") {
        return th.fg_mute;
    }
    if has(stack, "string") {
        return th.green;
    }
    if has(stack, "constant.numeric")
        || has(stack, "constant.character")
        || has(stack, "constant.language")
        || has(stack, "support.constant")
    {
        return th.orange;
    }
    if has(stack, "storage.type.function")
        || has(stack, "storage.modifier")
        || (has(stack, "keyword") && !has(stack, "keyword.operator"))
    {
        return th.keyword;
    }
    if has(stack, "entity.name.function") || has(stack, "support.function") {
        return th.md;
    }
    if has(stack, "variable.parameter") {
        return th.cyan;
    }
    if has(stack, "entity.name") || has(stack, "support.type") || has(stack, "storage.type") {
        return th.cyan;
    }
    th.fg
}

fn has(stack: &ScopeStack, needle: &str) -> bool {
    stack
        .as_slice()
        .iter()
        .any(|scope| scope.build_string().contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn rust_string_uses_green() {
        let th = Theme::night();
        let spans = line_spans("a.rs", r#"let s = "hi";"#, &th);
        assert!(
            spans
                .iter()
                .any(|(c, t)| *c == th.green && t.contains("hi")),
            "{spans:?}"
        );
    }

    #[test]
    fn rust_keyword_uses_keyword_color() {
        let th = Theme::night();
        let spans = line_spans("a.rs", "fn main() {}", &th);
        assert!(
            spans
                .iter()
                .any(|(c, t)| *c == th.keyword && t.contains("fn")),
            "{spans:?}"
        );
    }

    #[test]
    fn rust_comment_uses_mute() {
        let th = Theme::night();
        let spans = line_spans("a.rs", "// hi", &th);
        assert!(
            spans
                .iter()
                .any(|(c, t)| *c == th.fg_mute && t.contains("hi")),
            "{spans:?}"
        );
    }

    #[test]
    fn token_colors_come_from_theme() {
        let th = Theme::night();
        let allowed = [
            th.fg, th.fg_mute, th.green, th.orange, th.keyword, th.md, th.cyan,
        ];
        let line = r#"fn f(x: i32) -> i32 { let s = "hi"; s.len() + 1 } // c"#;
        for (c, tok) in line_spans("a.rs", line, &th) {
            assert!(
                allowed.contains(&c),
                "token {tok:?} used {c:?} which is not a theme color"
            );
        }
    }
}
