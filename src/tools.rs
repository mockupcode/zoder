use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;

use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::session::{AgentMode, Session, Todo};

pub fn definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a UTF-8 file. Use offset/limit for large files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "offset": { "type": "integer", "minimum": 1 },
                        "limit": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write",
                "description": "Create or overwrite a file with the given contents.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "contents": { "type": "string" }
                    },
                    "required": ["path", "contents"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search_replace",
                "description": "Replace exactly one occurrence of old_string with new_string in a file.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "old_string": { "type": "string" },
                        "new_string": { "type": "string" },
                        "replace_all": { "type": "boolean" }
                    },
                    "required": ["path", "old_string", "new_string"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "grep",
                "description": "Search file contents with a regular expression.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "path": { "type": "string" },
                        "glob": { "type": "string" }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "glob",
                "description": "Find files matching a glob pattern under the workspace.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List a directory. Hidden files are omitted unless requested.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Run a shell command in the workspace. Prefer this for git, builds, and tests.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "timeout_ms": { "type": "integer" }
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "todo_write",
                "description": "Replace the task list shown in the todos pane.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "content": { "type": "string" },
                                    "status": { "type": "string" }
                                },
                                "required": ["content", "status"]
                            }
                        }
                    },
                    "required": ["todos"]
                }
            }
        }
    ])
}

pub fn needs_permission(name: &str) -> bool {
    matches!(name, "bash" | "write" | "search_replace")
}

pub fn detail(name: &str, args: &Value) -> String {
    match name {
        "read_file" | "write" | "search_replace" | "list_dir" => args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(name)
            .to_string(),
        "bash" => args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or(name)
            .to_string(),
        "grep" => args
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or(name)
            .to_string(),
        "glob" => args
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or(name)
            .to_string(),
        "todo_write" => {
            let n = args
                .get("todos")
                .and_then(|t| t.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            format!("{n} items")
        }
        _ => args.to_string(),
    }
}

pub fn plan_forbidden(
    mode: AgentMode,
    name: &str,
    args: &Value,
    plan_path: &Path,
) -> Option<String> {
    if mode != AgentMode::Plan {
        return None;
    }
    match name {
        "read_file" | "grep" | "glob" | "list_dir" | "todo_write" => None,
        "bash" => {
            Some("Plan mode is read-only. Shell is blocked until the plan is approved.".into())
        }
        "write" | "search_replace" => {
            let p = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let resolved = PathBuf::from(p);
            if resolved == plan_path
                || Path::new(p).file_name() == Some(std::ffi::OsStr::new("plan.md"))
            {
                None
            } else {
                Some(format!(
                    "Plan mode can only edit {}. Other writes are rejected.",
                    plan_path.display()
                ))
            }
        }
        _ => Some("That tool is blocked in plan mode.".into()),
    }
}

pub fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn arg_path(session: &Session, args: &Value) -> Result<PathBuf, String> {
    arg_str(args, "path")
        .map(|p| resolve(&session.cwd, p))
        .ok_or_else(|| "path is required".into())
}

pub async fn execute(
    session: &mut Session,
    name: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> (bool, String) {
    if cancel.load(Ordering::Relaxed) {
        return (false, "cancelled".into());
    }
    match name {
        "read_file" => read_file(session, args),
        "write" => write_file(session, args),
        "search_replace" => search_replace(session, args),
        "grep" => grep(session, args),
        "glob" => glob_files(session, args),
        "list_dir" => list_dir(session, args),
        "bash" => bash(session, args, cancel).await,
        "todo_write" => todo_write(session, args),
        other => (false, format!("unknown tool: {other}")),
    }
}

fn read_file(session: &Session, args: &Value) -> (bool, String) {
    let path = match arg_path(session, args) {
        Ok(p) => p,
        Err(e) => return (false, e),
    };
    let offset = args
        .get("offset")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .max(1);
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(400) as usize;
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            let lines: Vec<&str> = s.lines().collect();
            let start = (offset as usize).saturating_sub(1).min(lines.len());
            let slice = &lines[start..lines.len().min(start + limit)];
            let mut out = String::new();
            for (i, line) in slice.iter().enumerate() {
                out.push_str(&format!("{:>4}→{}\n", start + i + 1, line));
            }
            if start + slice.len() < lines.len() {
                out.push_str(&format!(
                    "… {} more lines\n",
                    lines.len() - start - slice.len()
                ));
            }
            if out.is_empty() {
                out = "(empty file)\n".into();
            }
            (true, out)
        }
        Err(e) => (false, format!("{}: {e}", path.display())),
    }
}

fn write_file(session: &Session, args: &Value) -> (bool, String) {
    let path = match arg_path(session, args) {
        Ok(p) => p,
        Err(e) => return (false, e),
    };
    let contents = arg_str(args, "contents").unwrap_or("");
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return (false, e.to_string());
        }
    }
    match std::fs::write(&path, contents) {
        Ok(()) => (
            true,
            format!("wrote {} bytes to {}", contents.len(), path.display()),
        ),
        Err(e) => (false, e.to_string()),
    }
}

fn search_replace(session: &Session, args: &Value) -> (bool, String) {
    let path = match arg_path(session, args) {
        Ok(p) => p,
        Err(e) => return (false, e),
    };
    let old = arg_str(args, "old_string").unwrap_or("");
    let new = arg_str(args, "new_string").unwrap_or("");
    let all = args
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if old.is_empty() {
        return (false, "old_string is empty".into());
    }
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return (false, e.to_string()),
    };
    let count = src.matches(old).count();
    if count == 0 {
        return (false, "old_string not found".into());
    }
    if count > 1 && !all {
        return (
            false,
            format!("old_string found {count} times; pass replace_all or make it unique"),
        );
    }
    let next = if all {
        src.replace(old, new)
    } else {
        src.replacen(old, new, 1)
    };
    if let Err(e) = std::fs::write(&path, &next) {
        return (false, e.to_string());
    }
    (
        true,
        format!("updated {} ({count} replacement)", path.display()),
    )
}

fn grep(session: &Session, args: &Value) -> (bool, String) {
    let Some(pattern) = arg_str(args, "pattern") else {
        return (false, "pattern is required".into());
    };
    let re = match RegexBuilder::new(pattern).case_insensitive(true).build() {
        Ok(r) => r,
        Err(e) => return (false, e.to_string()),
    };
    let root = arg_str(args, "path")
        .map(|p| resolve(&session.cwd, p))
        .unwrap_or_else(|| session.cwd.clone());
    let glob = arg_str(args, "glob");
    let mut hits = Vec::new();
    let walker = WalkBuilder::new(&root)
        .hidden(false)
        .git_ignore(true)
        .max_depth(Some(12))
        .build();
    for ent in walker.flatten() {
        let path = ent.path();
        if !path.is_file() {
            continue;
        }
        if let Some(g) = glob {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if !glob_match(g, name) && !glob_match(g, &path.to_string_lossy()) {
                    continue;
                }
            }
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                hits.push(format!("{}:{}:{line}", path.display(), i + 1));
                if hits.len() >= 80 {
                    hits.push("… truncated".into());
                    return (true, hits.join("\n"));
                }
            }
        }
    }
    if hits.is_empty() {
        (true, "no matches".into())
    } else {
        (true, hits.join("\n"))
    }
}

fn glob_files(session: &Session, args: &Value) -> (bool, String) {
    let pattern = arg_str(args, "pattern").unwrap_or("*");
    let mut hits = Vec::new();
    for ent in WalkBuilder::new(&session.cwd)
        .git_ignore(true)
        .max_depth(Some(12))
        .build()
        .flatten()
    {
        let path = ent.path();
        if !path.is_file() {
            continue;
        }
        let rel = path.strip_prefix(&session.cwd).unwrap_or(path);
        let s = rel.to_string_lossy();
        if glob_match(pattern, &s)
            || path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| glob_match(pattern, n))
                .unwrap_or(false)
        {
            hits.push(s.to_string());
            if hits.len() >= 200 {
                break;
            }
        }
    }
    hits.sort();
    (
        true,
        if hits.is_empty() {
            "no files".into()
        } else {
            hits.join("\n")
        },
    )
}

fn list_dir(session: &Session, args: &Value) -> (bool, String) {
    let path = arg_str(args, "path")
        .map(|p| resolve(&session.cwd, p))
        .unwrap_or_else(|| session.cwd.clone());
    let rd = match std::fs::read_dir(&path) {
        Ok(r) => r,
        Err(e) => return (false, e.to_string()),
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() {
                format!("{n}/")
            } else {
                n
            }
        })
        .collect();
    names.sort();
    (
        true,
        if names.is_empty() {
            "(empty)".into()
        } else {
            names.join("\n")
        },
    )
}

async fn wait_cancel(flag: &AtomicBool) {
    loop {
        if flag.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

fn kill_child(child: &mut CommandChild) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.start_kill();
}

type CommandChild = tokio::process::Child;

async fn bash(session: &Session, args: &Value, cancel: &AtomicBool) -> (bool, String) {
    let Some(command) = arg_str(args, "command") else {
        return (false, "command is required".into());
    };
    let timeout = Duration::from_millis(
        args.get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(60_000)
            .clamp(1_000, 300_000),
    );
    let start = Instant::now();
    let mut cmd = Command::new("zsh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(&session.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (false, e.to_string()),
    };
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let read_out = async {
        let mut b = Vec::new();
        if let Some(ref mut r) = stdout {
            let _ = r.read_to_end(&mut b).await;
        }
        b
    };
    let read_err = async {
        let mut b = Vec::new();
        if let Some(ref mut r) = stderr {
            let _ = r.read_to_end(&mut b).await;
        }
        b
    };
    let (status, out, err) = tokio::select! {
        status = child.wait() => {
            let (out, err) = tokio::join!(read_out, read_err);
            (status, out, err)
        }
        _ = wait_cancel(cancel) => {
            kill_child(&mut child);
            let _ = child.wait().await;
            return (false, "cancelled".into());
        }
        _ = tokio::time::sleep(timeout) => {
            kill_child(&mut child);
            let _ = child.wait().await;
            return (false, format!("timed out after {timeout:?}"));
        }
    };
    let output = match status {
        Ok(s) => s,
        Err(e) => return (false, e.to_string()),
    };
    let mut text = String::from_utf8_lossy(&out).into_owned();
    if !err.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&err));
    }
    if text.len() > 80_000 {
        text.truncate(80_000);
        text.push_str("\n… truncated");
    }
    let code = output.code().unwrap_or(-1);
    let ms = start.elapsed().as_millis();
    let body = if text.trim().is_empty() {
        format!("exit {code} ({ms}ms)")
    } else {
        format!("{text}\nexit {code} ({ms}ms)")
    };
    (code == 0, body)
}

fn todo_write(session: &mut Session, args: &Value) -> (bool, String) {
    let Some(arr) = args.get("todos").and_then(|v| v.as_array()) else {
        return (false, "todos array required".into());
    };
    session.todos = arr
        .iter()
        .enumerate()
        .map(|(i, t)| Todo {
            id: t
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("t{i}")),
            content: t
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            status: t
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending")
                .to_string(),
        })
        .collect();
    (true, format!("{} todos", session.todos.len()))
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let pat = glob_to_regex(pattern);
    RegexBuilder::new(&pat)
        .case_insensitive(true)
        .build()
        .map(|r| r.is_match(text))
        .unwrap_or(false)
}

fn glob_to_regex(glob: &str) -> String {
    let mut out = String::from("^");
    let chars: Vec<char> = glob.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    out.push_str(".*");
                    i += 2;
                    if i < chars.len() && chars[i] == '/' {
                        i += 1;
                    }
                    continue;
                }
                out.push_str("[^/]*");
            }
            '?' => out.push_str("[^/]"),
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '[' | ']' | '\\' => {
                out.push('\\');
                out.push(chars[i]);
            }
            c => out.push(c),
        }
        i += 1;
    }
    out.push('$');
    out
}

pub fn index_files(cwd: &Path, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    for ent in WalkBuilder::new(cwd)
        .git_ignore(true)
        .hidden(false)
        .max_depth(Some(10))
        .build()
        .flatten()
    {
        let path = ent.path();
        if !path.is_file() {
            continue;
        }
        if let Ok(rel) = path.strip_prefix(cwd) {
            out.push(rel.to_string_lossy().into_owned());
            if out.len() >= cap {
                break;
            }
        }
    }
    out.sort();
    out
}

#[derive(Clone, Debug)]
pub struct FileHit {
    pub label: String,
    pub rel: String,
    pub is_dir: bool,
}

pub fn list_at_level(cwd: &Path, query: &str, limit: usize) -> Vec<FileHit> {
    let show_hidden = query.starts_with('!');
    let q = query.trim_start_matches('!');
    let (dir_rel, filter) = match q.rfind('/') {
        Some(i) => (&q[..=i], &q[i + 1..]),
        None => ("", q),
    };
    let root = if dir_rel.is_empty() {
        cwd.to_path_buf()
    } else {
        cwd.join(dir_rel.trim_end_matches('/'))
    };
    if !root.is_dir() {
        return Vec::new();
    }
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for ent in WalkBuilder::new(&root)
        .git_ignore(true)
        .hidden(!show_hidden)
        .max_depth(Some(1))
        .build()
        .flatten()
    {
        if ent.depth() == 0 {
            continue;
        }
        let path = ent.path();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        if crate::slash::match_indices(&name, filter).is_none() {
            continue;
        }
        let is_dir = path.is_dir();
        let rel = if dir_rel.is_empty() {
            if is_dir {
                format!("{name}/")
            } else {
                name.clone()
            }
        } else if is_dir {
            format!("{dir_rel}{name}/")
        } else {
            format!("{dir_rel}{name}")
        };
        let hit = FileHit {
            label: if is_dir { format!("{name}/") } else { name },
            rel,
            is_dir,
        };
        if is_dir {
            dirs.push(hit);
        } else {
            files.push(hit);
        }
    }
    dirs.sort_by(|a, b| a.label.cmp(&b.label));
    files.sort_by(|a, b| a.label.cmp(&b.label));
    dirs.append(&mut files);
    dirs.truncate(limit);
    dirs
}

pub fn fuzzy<'a>(query: &str, items: &'a [String], limit: usize) -> Vec<&'a str> {
    let q = query.to_lowercase();
    let mut scored: Vec<(i32, &str)> = items
        .iter()
        .filter_map(|s| {
            let l = s.to_lowercase();
            if q.is_empty() {
                return Some((0, s.as_str()));
            }
            if l.contains(&q) {
                return Some((100 - l.len() as i32, s.as_str()));
            }
            let mut it = l.chars();
            for ch in q.chars() {
                if !it.any(|c| c == ch) {
                    return None;
                }
            }
            Some((50 - l.len() as i32, s.as_str()))
        })
        .collect();
    scored.sort_by_key(|a| std::cmp::Reverse(a.0));
    scored.into_iter().take(limit).map(|(_, s)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_starstar_matches_nested() {
        assert!(glob_match("**/*.rs", "src/app.rs"));
        assert!(glob_match("*.rs", "app.rs"));
        assert!(!glob_match("*.rs", "src/app.rs"));
    }

    #[test]
    fn plan_mode_blocks_shell() {
        let p = PathBuf::from("/tmp/plan.md");
        let msg = plan_forbidden(AgentMode::Plan, "bash", &json!({"command":"ls"}), &p);
        assert!(msg.is_some());
        let ok = plan_forbidden(AgentMode::Plan, "read_file", &json!({"path":"a.rs"}), &p);
        assert!(ok.is_none());
    }

    #[test]
    fn search_replace_unique() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "alpha beta alpha").unwrap();
        let mut s = Session::new(dir.path().to_path_buf(), "m".into());
        let (ok, msg) = search_replace(
            &s,
            &json!({"path":"a.txt","old_string":"beta","new_string":"BETA"}),
        );
        assert!(ok, "{msg}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "alpha BETA alpha");
        let _ = &mut s;
    }

    #[test]
    fn list_at_level_dirs_then_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("lib.rs"), "x").unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "x").unwrap();
        let top = list_at_level(dir.path(), "", 50);
        let labels: Vec<_> = top.iter().map(|h| h.label.as_str()).collect();
        assert!(labels.contains(&"src/"), "{labels:?}");
        assert!(labels.contains(&"Cargo.toml"), "{labels:?}");
        assert!(top.iter().any(|h| h.is_dir && h.rel == "src/"));
        let inner = list_at_level(dir.path(), "src/", 50);
        assert!(
            inner.iter().any(|h| !h.is_dir && h.rel == "src/lib.rs"),
            "{inner:?}"
        );
        let filtered = list_at_level(dir.path(), "src/li", 50);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].rel, "src/lib.rs");
    }

    #[tokio::test]
    async fn bash_stops_on_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new(dir.path().to_path_buf(), "m".into());
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let cwd = dir.path().to_path_buf();
        let task = tokio::spawn(async move {
            let mut session = Session::new(cwd, "m".into());
            execute(&mut session, "bash", &json!({"command": "sleep 30"}), &flag).await
        });
        tokio::time::sleep(Duration::from_millis(80)).await;
        cancel.store(true, Ordering::Relaxed);
        let (ok, out) = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("bash did not stop")
            .expect("join");
        assert!(!ok, "{out}");
        assert!(out.contains("cancelled"), "{out}");
        let _ = &mut s;
    }
}
