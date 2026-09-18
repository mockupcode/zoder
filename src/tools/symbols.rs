use ignore::WalkBuilder;
use regex::Regex;
use serde_json::Value;

use crate::session::Session;

use super::{arg_path, arg_str, edit::edit_diff};

pub fn lsp_replace_symbol(session: &Session, args: &Value) -> (bool, String) {
    let path = match arg_path(session, args) {
        Ok(p) => p,
        Err(e) => return (false, e),
    };
    let Some(symbol) = arg_str(args, "symbol") else {
        return (false, "symbol is required".into());
    };
    let action = arg_str(args, "action").unwrap_or("replace");
    let text = arg_str(args, "content")
        .or_else(|| arg_str(args, "text"))
        .unwrap_or("");
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return (false, e.to_string()),
    };
    let Some((start, end)) = symbol_span(&src, symbol) else {
        return (false, format!("Symbol '{symbol}' not found"));
    };
    let next = match action {
        "delete" => format!("{}{}", &src[..start], &src[end..]),
        "add_before" => format!("{}{}{}", &src[..start], text, &src[start..]),
        "add_after" => format!("{}{}{}", &src[..end], text, &src[end..]),
        _ => format!("{}{}{}", &src[..start], text, &src[end..]),
    };
    if let Err(e) = std::fs::write(&path, &next) {
        return (false, e.to_string());
    }
    (true, edit_diff(&src, &src[start..end], text))
}

pub fn lsp_rename(session: &Session, args: &Value) -> (bool, String) {
    let Some(symbol) = arg_str(args, "symbol") else {
        return (false, "symbol is required".into());
    };
    let Some(new_name) = arg_str(args, "new_name") else {
        return (false, "new_name is required".into());
    };
    let root = arg_str(args, "path")
        .map(|p| super::resolve(&session.cwd, p))
        .unwrap_or_else(|| session.cwd.clone());
    let Ok(re) = Regex::new(&format!(r"\b{}\b", regex::escape(symbol))) else {
        return (false, "invalid symbol".into());
    };
    let mut changed = Vec::new();
    for ent in WalkBuilder::new(&root)
        .git_ignore(true)
        .max_depth(Some(12))
        .build()
        .flatten()
    {
        let path = ent.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !matches!(
            ext,
            "rs" | "go"
                | "py"
                | "js"
                | "ts"
                | "tsx"
                | "jsx"
                | "c"
                | "h"
                | "cc"
                | "cpp"
                | "java"
                | "kt"
                | "swift"
        ) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        if !re.is_match(&src) {
            continue;
        }
        let next = re.replace_all(&src, new_name);
        if next.as_ref() != src && std::fs::write(path, next.as_ref()).is_ok() {
            changed.push(
                path.strip_prefix(&session.cwd)
                    .unwrap_or(path)
                    .display()
                    .to_string(),
            );
        }
    }
    if changed.is_empty() {
        return (
            false,
            format!("No rename edits generated for symbol '{symbol}'"),
        );
    }
    let mut out = format!(
        "Renamed '{symbol}' to '{new_name}' in {} file(s):\n\n",
        changed.len()
    );
    for f in &changed {
        out.push_str(&format!("  {f}\n"));
    }
    (true, out)
}

fn symbol_span(src: &str, name: &str) -> Option<(usize, usize)> {
    let escaped = regex::escape(name);
    let re = Regex::new(&format!(
        r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:fn|struct|enum|type|trait|impl|class|interface|def)\s+{escaped}\b"
    ))
    .ok()?;
    let m = re.find(src)?;
    let start = m.start();
    let rest = &src[m.end()..];
    if let Some(rel) = rest.find('{') {
        let abs = m.end() + rel;
        let end = match_brace(src, abs)? + 1;
        return Some((start, end));
    }
    if rest.trim_start().starts_with(':') {
        let indent = src[start..m.end()]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .count();
        let after = m.end();
        let mut end = src.len();
        let mut seen = false;
        for (i, line) in src[after..].split_inclusive('\n').enumerate() {
            if i == 0 {
                continue;
            }
            let t = line.trim_start_matches([' ', '\t', '\n']);
            if t.is_empty() {
                continue;
            }
            let lead = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
            if seen && lead <= indent {
                end = after + src[after..].find(line).unwrap_or(0);
                break;
            }
            seen = true;
        }
        return Some((start, end));
    }
    Some((start, m.end()))
}

pub fn lsp_definition(session: &Session, args: &Value) -> (bool, String) {
    let Some(symbol) = arg_str(args, "symbol") else {
        return (false, "symbol is required".into());
    };
    let root = arg_str(args, "path")
        .map(|p| super::resolve(&session.cwd, p))
        .unwrap_or_else(|| session.cwd.clone());
    for ent in WalkBuilder::new(&root)
        .git_ignore(true)
        .max_depth(Some(12))
        .build()
        .flatten()
    {
        let path = ent.path();
        if !path.is_file() {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        if let Some((start, end)) = symbol_span(&src, symbol) {
            let line = src[..start].bytes().filter(|&b| b == b'\n').count() + 1;
            let rel = path.strip_prefix(&session.cwd).unwrap_or(path);
            let snippet = src.get(start..end.min(start + 800)).unwrap_or("");
            return (true, format!("{}:{line}\n\n{snippet}", rel.display()));
        }
    }
    (false, format!("Symbol '{symbol}' not found"))
}

pub fn lsp_symbols(session: &Session, args: &Value) -> (bool, String) {
    let path = match arg_path(session, args) {
        Ok(p) => p,
        Err(e) => return (false, e),
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return (false, e.to_string()),
    };
    let Ok(re) = Regex::new(
        r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(fn|struct|enum|type|trait|impl|class|interface|def)\s+(\w+)",
    ) else {
        return (false, "regex failed".into());
    };
    let mut out = String::new();
    for cap in re.captures_iter(&src) {
        let kind = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let name = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let start = cap.get(0).map(|m| m.start()).unwrap_or(0);
        let line = src[..start].bytes().filter(|&b| b == b'\n').count() + 1;
        let end_line = symbol_span(&src, name)
            .map(|(_, e)| src[..e].bytes().filter(|&b| b == b'\n').count() + 1)
            .unwrap_or(line);
        out.push_str(&format!("{line}-{end_line}  {kind} {name}\n"));
    }
    if out.is_empty() {
        (true, "No symbols found.".into())
    } else {
        (true, out)
    }
}

pub fn references(session: &Session, args: &Value) -> (bool, String) {
    let Some(symbol) = arg_str(args, "symbol") else {
        return (false, "symbol is required".into());
    };
    let root = arg_str(args, "path")
        .map(|p| super::resolve(&session.cwd, p))
        .unwrap_or_else(|| session.cwd.clone());
    let Ok(re) = Regex::new(&format!(r"\b{}\b", regex::escape(symbol))) else {
        return (false, "invalid symbol".into());
    };
    let mut hits = Vec::new();
    for ent in WalkBuilder::new(&root)
        .git_ignore(true)
        .max_depth(Some(12))
        .build()
        .flatten()
    {
        let path = ent.path();
        if !path.is_file() {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path.strip_prefix(&session.cwd).unwrap_or(path);
        for (i, line) in src.lines().enumerate() {
            if re.is_match(line) {
                hits.push(format!("{}:{}:{line}", rel.display(), i + 1));
                if hits.len() >= 80 {
                    hits.push("… truncated".into());
                    return (true, hits.join("\n"));
                }
            }
        }
    }
    if hits.is_empty() {
        (true, format!("No references to '{symbol}'"))
    } else {
        (true, hits.join("\n"))
    }
}

pub fn lsp_call_hierarchy(session: &Session, args: &Value) -> (bool, String) {
    let Some(symbol) = arg_str(args, "symbol") else {
        return (false, "symbol is required".into());
    };
    let direction = arg_str(args, "direction").unwrap_or("incoming");
    if direction == "outgoing" {
        let path = match arg_path(session, args) {
            Ok(p) => p,
            Err(_) => return references(session, args),
        };
        let Ok(src) = std::fs::read_to_string(&path) else {
            return references(session, args);
        };
        let Some((start, end)) = symbol_span(&src, symbol) else {
            return (false, format!("Symbol '{symbol}' not found"));
        };
        let body = &src[start..end];
        let Ok(re) = Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(") else {
            return (false, "regex failed".into());
        };
        let mut names = Vec::new();
        for cap in re.captures_iter(body) {
            let n = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            if n != symbol && !names.iter().any(|x| x == n) {
                names.push(n.to_string());
            }
        }
        if names.is_empty() {
            return (true, format!("No outgoing calls from '{symbol}'"));
        }
        return (
            true,
            format!("Outgoing from '{symbol}':\n  {}", names.join("\n  ")),
        );
    }
    references(session, args)
}

pub fn diagnostics(session: &Session, args: &Value) -> (bool, String) {
    let path = arg_str(args, "file_path").or_else(|| arg_str(args, "path"));
    let cwd = session.cwd.clone();
    if !cwd.join("Cargo.toml").exists() {
        return (
            true,
            "No language server attached. For Rust projects, add Cargo.toml or run cargo clippy via bash.".into(),
        );
    }
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(["check", "--offline", "--message-format=short"])
        .current_dir(&cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(p) = path {
        let _ = p;
    }
    match cmd.output() {
        Ok(o) => {
            let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
            let e = String::from_utf8_lossy(&o.stderr);
            if !e.is_empty() {
                if !t.is_empty() {
                    t.push('\n');
                }
                t.push_str(&e);
            }
            if t.trim().is_empty() {
                (true, "No diagnostics.".into())
            } else {
                if t.len() > 20_000 {
                    t.truncate(20_000);
                    t.push_str("\n… truncated");
                }
                (true, t)
            }
        }
        Err(e) => (false, e.to_string()),
    }
}

fn match_brace(src: &str, open: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        return None;
    }
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'"' | b'\'' => {
                let q = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != q {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}
