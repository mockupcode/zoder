use serde_json::Value;

use crate::session::Session;

use super::{arg_bool, arg_path, arg_str, EDIT_DIFF};

pub fn write_file(session: &Session, args: &Value) -> (bool, String) {
    let path = match arg_path(session, args) {
        Ok(p) => p,
        Err(e) => return (false, e),
    };
    let contents = arg_str(args, "content")
        .or_else(|| arg_str(args, "contents"))
        .unwrap_or("");
    let prev = std::fs::read_to_string(&path).unwrap_or_default();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return (false, e.to_string());
        }
    }
    match std::fs::write(&path, contents) {
        Ok(()) => (true, edit_diff(&prev, &prev, contents)),
        Err(e) => (false, e.to_string()),
    }
}

pub fn edit(session: &Session, args: &Value) -> (bool, String) {
    let path = match arg_path(session, args) {
        Ok(p) => p,
        Err(e) => return (false, e),
    };
    let old = arg_str(args, "old_string").unwrap_or("");
    let new = arg_str(args, "new_string").unwrap_or("");
    let all = arg_bool(args, "replace_all").unwrap_or(false);
    if old.is_empty() {
        if new.is_empty() {
            return (false, "old_string is empty".into());
        }
        if path.exists() {
            return (false, format!("file already exists: {}", path.display()));
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        return match std::fs::write(&path, new) {
            Ok(()) => (true, edit_diff("", "", new)),
            Err(e) => (false, e.to_string()),
        };
    }
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return (false, e.to_string()),
    };
    let (next, note) = match find_and_replace(&src, old, new, all) {
        Ok(v) => v,
        Err(e) => return (false, e),
    };
    if next == src {
        return (
            false,
            "new content is the same as old content. No changes made.".into(),
        );
    }
    if let Err(e) = std::fs::write(&path, &next) {
        return (false, e.to_string());
    }
    let mut out = if src.contains(old) {
        edit_diff(&src, old, new)
    } else {
        edit_diff(&src, &src, &next)
    };
    if !note.is_empty() {
        out.push('\n');
        out.push_str(&note);
    }
    (true, out)
}

pub fn multiedit(session: &Session, args: &Value) -> (bool, String) {
    let path = match arg_path(session, args) {
        Ok(p) => p,
        Err(e) => return (false, e),
    };
    let Some(edits) = args.get("edits").and_then(|v| v.as_array()) else {
        return (false, "at least one edit operation is required".into());
    };
    if edits.is_empty() {
        return (false, "at least one edit operation is required".into());
    }
    let mut src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) if arg_str(&edits[0], "old_string").unwrap_or("").is_empty() => String::new(),
        Err(e) => return (false, e.to_string()),
    };
    let original = src.clone();
    let mut failed = 0usize;
    let mut note = false;
    for (i, ed) in edits.iter().enumerate() {
        let old = arg_str(ed, "old_string").unwrap_or("");
        let new = arg_str(ed, "new_string").unwrap_or("");
        let all = arg_bool(ed, "replace_all").unwrap_or(false);
        if old.is_empty() {
            if i == 0 && src.is_empty() {
                src = new.to_string();
                continue;
            }
            failed += 1;
            continue;
        }
        match find_and_replace(&src, old, new, all) {
            Ok((next, n)) => {
                if !n.is_empty() {
                    note = true;
                }
                src = next;
            }
            Err(_) => failed += 1,
        }
    }
    if src == original {
        return (false, format!("no changes made ({failed} edit(s) failed)"));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, &src) {
        return (false, e.to_string());
    }
    let mut out = edit_diff(&original, &original, &src);
    if note {
        out.push('\n');
        out.push_str(WHITESPACE_NOTE);
    }
    if failed > 0 {
        out.push_str(&format!("\n{failed} edit(s) failed"));
    }
    (true, out)
}

const WHITESPACE_NOTE: &str = "Note: old_string did not match exactly. The edit was applied to whitespace-equivalent text in the file and new_string was re-indented to match the file's style. Verify the result.";

fn find_and_replace(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, String), String> {
    let count = content.matches(old).count();
    if count == 1 || (replace_all && count > 0) {
        let next = if replace_all {
            content.replace(old, new)
        } else {
            content.replacen(old, new, 1)
        };
        return Ok((next, String::new()));
    }
    if count > 1 {
        return Err(format!(
            "old_string appears {count} times. Provide more context or set replace_all to true"
        ));
    }
    if let Some(next) = normalized_replace(content, old, new, replace_all) {
        return Ok((next, WHITESPACE_NOTE.into()));
    }
    Err(not_found(content, old))
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn join_normalized(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|l| normalize_ws(l))
        .collect::<Vec<_>>()
        .join("\n")
}

fn find_normalized_matches(content: &str, old: &str) -> Vec<(usize, usize)> {
    let content_lines: Vec<&str> = content.split('\n').collect();
    let old_lines: Vec<&str> = old.split('\n').collect();
    let norm_old = join_normalized(&old_lines);
    if norm_old.trim().is_empty() {
        return Vec::new();
    }
    let norm_content_lines: Vec<String> = content_lines.iter().map(|l| normalize_ws(l)).collect();
    let norm_content = norm_content_lines.join("\n");
    let mut matches = Vec::new();
    let mut search_from = 0usize;
    while search_from <= norm_content.len() {
        let Some(idx) = norm_content[search_from..].find(&norm_old) else {
            break;
        };
        let abs = search_from + idx;
        let end = abs + norm_old.len();
        let at_start = abs == 0 || norm_content.as_bytes().get(abs - 1) == Some(&b'\n');
        let at_end = end == norm_content.len() || norm_content.as_bytes().get(end) == Some(&b'\n');
        if at_start && at_end {
            let start_line = line_at(&norm_content_lines, abs);
            let end_line = start_line + old_lines.len() - 1;
            if end_line < content_lines.len() {
                matches.push((start_line, end_line));
            }
            search_from = end + 1;
        } else {
            search_from = abs + 1;
        }
    }
    matches
}

fn line_at(lines: &[String], offset: usize) -> usize {
    let mut pos = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if pos + line.len() >= offset {
            return i;
        }
        pos += line.len() + 1;
    }
    lines.len().saturating_sub(1)
}

fn normalized_replace(content: &str, old: &str, new: &str, replace_all: bool) -> Option<String> {
    let matches = find_normalized_matches(content, old);
    if matches.is_empty() {
        return None;
    }
    if !replace_all && matches.len() > 1 {
        return None;
    }
    let content_lines: Vec<&str> = content.split('\n').collect();
    let file_unit = detect_indent_unit(&content_lines);
    let mut result: Vec<String> = content_lines.iter().map(|s| (*s).to_string()).collect();
    for &(start, end) in matches.iter().rev() {
        let actual = content_lines[start..=end].join("\n");
        let adapted = adapt_indentation(&actual, old, new, &file_unit);
        let adapted_lines: Vec<String> = adapted.split('\n').map(|s| s.to_string()).collect();
        result.splice(start..=end, adapted_lines);
    }
    Some(result.join("\n"))
}

fn detect_indent_unit(lines: &[&str]) -> String {
    let mut min_spaces = 0usize;
    let mut has_tabs = false;
    for line in lines {
        let trimmed = line.trim_start_matches([' ', '\t']);
        if trimmed.is_empty() {
            continue;
        }
        let leading = &line[..line.len() - trimmed.len()];
        if leading.is_empty() {
            continue;
        }
        if leading.contains('\t') {
            has_tabs = true;
            break;
        }
        let n = leading.chars().filter(|c| *c == ' ').count();
        if n > 0 && (min_spaces == 0 || n < min_spaces) {
            min_spaces = n;
        }
    }
    if has_tabs {
        "\t".into()
    } else if min_spaces > 0 {
        " ".repeat(min_spaces)
    } else {
        String::new()
    }
}

fn measure_depth(leading: &str, unit: &str) -> i32 {
    if unit.is_empty() {
        return 0;
    }
    if unit == "\t" {
        return leading.matches('\t').count() as i32;
    }
    (leading.matches(' ').count() / unit.len()) as i32
}

fn first_indent_depth(lines: &[&str], unit: &str) -> i32 {
    for l in lines {
        let trimmed = l.trim_start_matches([' ', '\t']);
        if trimmed.is_empty() {
            continue;
        }
        let leading = &l[..l.len() - trimmed.len()];
        return measure_depth(leading, unit);
    }
    0
}

fn adapt_indentation(actual: &str, old: &str, new: &str, file_unit: &str) -> String {
    if file_unit.is_empty() {
        return new.to_string();
    }
    let actual_lines: Vec<&str> = actual.split('\n').collect();
    let old_lines: Vec<&str> = old.split('\n').collect();
    let new_lines: Vec<&str> = new.split('\n').collect();
    let mut source_unit = detect_indent_unit(&old_lines);
    if source_unit.is_empty() {
        source_unit = detect_indent_unit(&new_lines);
    }
    if source_unit.is_empty() {
        source_unit = file_unit.to_string();
    }
    let base = first_indent_depth(&actual_lines, file_unit);
    let old_base = first_indent_depth(&old_lines, &source_unit);
    let offset = base - old_base;
    if source_unit == file_unit && offset == 0 {
        return new.to_string();
    }
    new_lines
        .iter()
        .map(|line| {
            let trimmed = line.trim_start_matches([' ', '\t']);
            if trimmed.is_empty() {
                return (*line).to_string();
            }
            let leading = &line[..line.len() - trimmed.len()];
            let depth = (measure_depth(leading, &source_unit) + offset).max(0) as usize;
            format!("{}{trimmed}", file_unit.repeat(depth))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn visualize_ws(s: &str) -> String {
    let s = s.replace('\t', "→");
    let trimmed = s.trim_start_matches(' ');
    let leading = s.len() - trimmed.len();
    format!("{}{trimmed}", "·".repeat(leading))
}

fn not_found(content: &str, old: &str) -> String {
    let mut msg = String::from(
        "old_string not found in file. Make sure it matches exactly, including whitespace and line breaks",
    );
    if let Some(hint) = diagnose(content, old) {
        msg.push_str("\n\n");
        msg.push_str(&hint);
    }
    msg
}

fn diagnose(content: &str, old: &str) -> Option<String> {
    let content_lines: Vec<&str> = content.split('\n').collect();
    let old_lines: Vec<&str> = old.split('\n').collect();
    let matches = find_normalized_matches(content, old);
    if let Some(&(start, end)) = matches.first() {
        let mut b = String::from("A whitespace-normalized match was found, so the text exists but with different whitespace (tabs vs spaces, or different indentation).\n");
        b.push_str(&format!(
            "Actual file content (lines {}-{}):\n",
            start + 1,
            end + 1
        ));
        for (i, line) in content_lines
            .iter()
            .enumerate()
            .take(end.min(content_lines.len().saturating_sub(1)) + 1)
            .skip(start)
        {
            b.push_str(&format!("{:6}|{}\n", i + 1, visualize_ws(line)));
        }
        b.push_str("Use the exact whitespace shown above (→ = tab, · = space).");
        return Some(b);
    }
    let trimmed_old: Vec<&str> = old_lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    if trimmed_old.is_empty() || content_lines.is_empty() {
        return None;
    }
    let window = trimmed_old.len();
    if content_lines.len() < window {
        return None;
    }
    let mut best = 0;
    let mut best_start = 0;
    for start in 0..=content_lines.len() - window {
        let score = (0..window)
            .filter(|j| content_lines[start + j].trim() == trimmed_old[*j])
            .count();
        if score > best {
            best = score;
            best_start = start;
        }
    }
    if best < window.div_ceil(2) {
        return None;
    }
    let end = best_start + window - 1;
    let ctx_s = best_start.saturating_sub(1);
    let ctx_e = (end + 1).min(content_lines.len() - 1);
    let mut b = format!(
        "No exact match found. Closest match at lines {}-{} ({best}/{window} lines match after trimming whitespace):\n",
        best_start + 1,
        end + 1
    );
    for (i, line) in content_lines.iter().enumerate().take(ctx_e + 1).skip(ctx_s) {
        b.push_str(&format!("{:6}|{}\n", i + 1, visualize_ws(line)));
    }
    b.push_str("Use the exact text shown above (→ = tab, · = space).");
    Some(b)
}

pub fn edit_diff(src: &str, old: &str, new: &str) -> String {
    let Some(at) = src
        .find(old)
        .or(if old.is_empty() { Some(0) } else { None })
    else {
        return format_hunk(1, &line_diff(&[], &split_lines(new)));
    };
    let line_begin = src[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = at + old.len();
    let line_end = src[end..].find('\n').map(|i| end + i).unwrap_or(src.len());
    let start_ln = src[..line_begin].bytes().filter(|&b| b == b'\n').count() + 1;
    let prefix = &src[line_begin..at];
    let suffix = &src[end..line_end];
    let old_region = &src[line_begin..line_end];
    let new_region = format!("{prefix}{new}{suffix}");
    let ctx = 3usize;
    let file: Vec<&str> = src.lines().collect();
    let before = {
        let i = start_ln.saturating_sub(1);
        let from = i.saturating_sub(ctx);
        &file[from..i]
    };
    let old_ln_count = old_region.lines().count().max(1);
    let after_idx = (start_ln - 1 + old_ln_count).min(file.len());
    let after = {
        let to = (after_idx + ctx).min(file.len());
        &file[after_idx..to]
    };
    let mut ops = Vec::new();
    for line in before {
        ops.push((b' ', (*line).to_string()));
    }
    ops.extend(line_diff(
        &split_lines(old_region),
        &split_lines(&new_region),
    ));
    for line in after {
        ops.push((b' ', (*line).to_string()));
    }
    format_hunk(start_ln.saturating_sub(before.len()), &ops)
}

fn split_lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.lines().collect()
    }
}

fn line_diff(a: &[&str], b: &[&str]) -> Vec<(u8, String)> {
    let (n, m) = (a.len(), b.len());
    if n == 0 && m == 0 {
        return Vec::new();
    }
    if n.saturating_mul(m) > 80_000 {
        let mut v: Vec<(u8, String)> = a.iter().map(|s| (b'-', (*s).to_string())).collect();
        v.extend(b.iter().map(|s| (b'+', (*s).to_string())));
        return v;
    }
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            dp[i + 1][j + 1] = if a[i] == b[j] {
                dp[i][j] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            out.push((b' ', a[i - 1].to_string()));
            i -= 1;
            j -= 1;
        } else if dp[i][j - 1] >= dp[i - 1][j] {
            out.push((b'+', b[j - 1].to_string()));
            j -= 1;
        } else {
            out.push((b'-', a[i - 1].to_string()));
            i -= 1;
        }
    }
    while i > 0 {
        out.push((b'-', a[i - 1].to_string()));
        i -= 1;
    }
    while j > 0 {
        out.push((b'+', b[j - 1].to_string()));
        j -= 1;
    }
    out.reverse();
    out
}

fn format_hunk(start: usize, ops: &[(u8, String)]) -> String {
    let mut old_ln = start;
    let mut new_ln = start;
    let mut out = String::from(EDIT_DIFF);
    out.push('\n');
    for (tag, text) in ops {
        let num = match *tag {
            b'+' => {
                let n = new_ln;
                new_ln += 1;
                n
            }
            b'-' => {
                let n = old_ln;
                old_ln += 1;
                n
            }
            _ => {
                let n = new_ln;
                old_ln += 1;
                new_ln += 1;
                n
            }
        };
        out.push(*tag as char);
        out.push_str(&format!("{num:>4}|{text}\n"));
    }
    out
}

pub fn parse_edit_diff(s: &str) -> Option<Vec<(char, u32, String)>> {
    let mut lines = s.lines();
    if lines.next()? != EDIT_DIFF {
        return None;
    }
    let mut rows = Vec::new();
    for line in lines {
        if line.starts_with("Note:") || line.contains("failed") {
            continue;
        }
        let mut chars = line.chars();
        let tag = chars.next()?;
        if !matches!(tag, '+' | '-' | ' ') {
            continue;
        }
        let rest: String = chars.collect();
        let Some((num, text)) = rest.split_once('|') else {
            continue;
        };
        let Ok(n) = num.trim().parse() else {
            continue;
        };
        rows.push((tag, n, text.to_string()));
    }
    if rows.is_empty() {
        None
    } else {
        Some(rows)
    }
}
