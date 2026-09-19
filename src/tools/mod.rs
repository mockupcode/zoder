mod edit;
mod jobs;
mod symbols;
mod web;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde_json::{json, Value};

use crate::session::{Session, Todo};

pub use edit::parse_edit_diff;
pub const EDIT_DIFF: &str = "DIFF";

pub fn definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "view",
                "description": "Read a file by path with line numbers; supports offset and line limit. Use ls for directories.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string" },
                        "path": { "type": "string" },
                        "offset": { "type": "integer", "minimum": 1 },
                        "limit": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["file_path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "ls",
                "description": "List files and directories as a tree; skips hidden files and common system dirs. Use glob to find files by pattern, grep to search contents.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "glob",
                "description": "Find files by name/pattern (glob syntax), sorted by modification time; skips hidden files. Use grep to search file contents.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "grep",
                "description": "Search file contents by regex or literal text; returns matching file paths sorted by modification time; respects .gitignore. Use glob to filter by filename, not contents.",
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
                "name": "search",
                "description": "Search the public web. Returns titles, URLs, and snippets.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "count": { "type": "integer" }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "sourcegraph",
                "description": "Search code across public GitHub repositories via Sourcegraph; supports regex, language/repo/file filters, and symbol search (max 20 results). Only searches public repos.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "count": { "type": "integer" },
                        "context_window": { "type": "integer" },
                        "timeout": { "type": "integer" }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Edit a file by exact find-and-replace; can also create or delete content. If old_string differs from the file only in whitespace, the matching lines are still edited and new_string is re-indented to the file's style. For whole-function/method/type replacements prefer lsp_replace_symbol. For renames prefer lsp_rename. For large edits use write.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string" },
                        "path": { "type": "string" },
                        "old_string": { "type": "string" },
                        "new_string": { "type": "string" },
                        "replace_all": { "type": "boolean" }
                    },
                    "required": ["file_path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "multiedit",
                "description": "Apply multiple find-and-replace edits to a single file in one operation; edits run sequentially. Prefer over edit for multiple changes to the same file. Same exact-match rules as edit apply.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string" },
                        "path": { "type": "string" },
                        "edits": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "old_string": { "type": "string" },
                                    "new_string": { "type": "string" },
                                    "replace_all": { "type": "boolean" }
                                },
                                "required": ["old_string", "new_string"]
                            }
                        }
                    },
                    "required": ["file_path", "edits"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write",
                "description": "Create or overwrite a file with given content; auto-creates parent dirs. Cannot append. Read the file first to avoid conflicts. For surgical changes use edit or multiedit.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string" },
                        "path": { "type": "string" },
                        "content": { "type": "string" },
                        "contents": { "type": "string" }
                    },
                    "required": ["file_path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "lsp_replace_symbol",
                "description": "Replace, insert, or delete an entire symbol (function, method, class, struct) by name using language-aware ranges. Prefer this over edit for whole-symbol changes. Actions: replace (default), add_before, add_after, delete.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string" },
                        "path": { "type": "string" },
                        "symbol": { "type": "string" },
                        "action": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["file_path", "symbol"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "lsp_rename",
                "description": "Rename a symbol across all files. Prefer this over manual multi-file edit for any rename. Returns the list of changed files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "symbol": { "type": "string" },
                        "new_name": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["symbol", "new_name"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Execute shell commands. Long-running commands automatically move to background and return a shell ID. IMPORTANT: Use grep/glob instead of find/grep. Use view/ls instead of cat/head/tail/ls. Do not use sed/awk/python to edit files — use edit, multiedit, or write. File operations should not use run_in_background. Use job_output to read background output and job_kill to stop a job.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "description": { "type": "string" },
                        "working_dir": { "type": "string" },
                        "timeout_ms": { "type": "integer" },
                        "run_in_background": { "type": "boolean" },
                        "auto_background_after": { "type": "integer" }
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "job_output",
                "description": "Get stdout/stderr from a background shell by ID; set wait=true to block until completion.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "shell_id": { "type": "string" },
                        "wait": { "type": "boolean" }
                    },
                    "required": ["shell_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "job_kill",
                "description": "Terminate a background shell process by ID.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "shell_id": { "type": "string" }
                    },
                    "required": ["shell_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web via DuckDuckGo; returns titles, URLs, and snippets. Follow up with web_fetch to get full page content.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "max_results": { "type": "integer" },
                        "count": { "type": "integer" }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "web_fetch",
                "description": "Fetch a web URL and return content as markdown. Large pages (>50KB) are saved to a temp file for grep/view.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" }
                    },
                    "required": ["url"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "fetch",
                "description": "Fetch raw content from a URL as text, markdown, or html (max 100KB); no AI processing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" },
                        "format": { "type": "string" },
                        "timeout": { "type": "integer" }
                    },
                    "required": ["url", "format"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "download",
                "description": "Download a URL directly to a local file (binary-safe, streaming, max 600s timeout); overwrites without warning. For reading content into context use fetch.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" },
                        "file_path": { "type": "string" },
                        "timeout": { "type": "integer" }
                    },
                    "required": ["url", "file_path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "lsp_definition",
                "description": "Find the definition of a symbol by name. Prefer this over grep for finding where something is defined. Returns the file path, line number, and surrounding context. Complements references (which finds usages).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "symbol": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["symbol"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "lsp_symbols",
                "description": "List document symbols (functions, types, methods) for a file. Returns names, kinds, and line ranges. Use before editing unfamiliar files or to find the exact name for lsp_replace_symbol.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["file_path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "references",
                "description": "Find all references to a symbol by name; more accurate than grep for code symbols.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "symbol": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["symbol"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "lsp_call_hierarchy",
                "description": "Show the call hierarchy for a symbol — incoming calls (who calls this) or outgoing calls (what does this call). direction: incoming (default) or outgoing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "symbol": { "type": "string" },
                        "direction": { "type": "string" },
                        "file_path": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["symbol"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "diagnostics",
                "description": "Get errors, warnings, and hints for a file or the whole project.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string" },
                        "path": { "type": "string" }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "question",
                "description": "Ask the user a structured question and wait for their response. Use when you need clarification, confirmation, or a choice before proceeding. types: yes_no, single_choice, multi_choice, free_text. Each question needs question and description. Max 5 questions. Max 5 choices.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "questions": { "type": "array" },
                        "confirm_title": { "type": "string" },
                        "confirm_description": { "type": "string" }
                    },
                    "required": ["questions"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "todos",
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

pub fn canonicalize(name: &str) -> &str {
    match name {
        "web_search" => "search",
        n => n,
    }
}

pub fn detail(name: &str, args: &Value) -> String {
    match canonicalize(name) {
        "view" | "write" | "edit" | "multiedit" | "ls" | "lsp_replace_symbol" => {
            arg_str(args, "file_path")
                .or_else(|| arg_str(args, "path"))
                .unwrap_or(name)
                .to_string()
        }
        "bash" => arg_str(args, "command")
            .or_else(|| arg_str(args, "description"))
            .unwrap_or(name)
            .to_string(),
        "grep" | "glob" | "search" | "sourcegraph" => arg_str(args, "pattern")
            .or_else(|| arg_str(args, "query"))
            .unwrap_or(name)
            .to_string(),
        "fetch" | "web_fetch" | "download" => arg_str(args, "url").unwrap_or(name).to_string(),
        "lsp_definition" | "references" | "lsp_call_hierarchy" | "lsp_symbols" => {
            arg_str(args, "symbol")
                .or_else(|| arg_str(args, "file_path"))
                .or_else(|| arg_str(args, "path"))
                .unwrap_or(name)
                .to_string()
        }
        "question" => "ask user".into(),
        "diagnostics" => arg_str(args, "file_path")
            .or_else(|| arg_str(args, "path"))
            .unwrap_or("project")
            .to_string(),
        "job_output" | "job_kill" => arg_str(args, "shell_id").unwrap_or(name).to_string(),
        "lsp_rename" => format!(
            "{} → {}",
            arg_str(args, "symbol").unwrap_or("?"),
            arg_str(args, "new_name").unwrap_or("?")
        ),
        "todos" => {
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

pub fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

pub(crate) fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

pub(crate) fn arg_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

pub(crate) fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key)
        .and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|n| n as u64)))
}

pub(crate) fn arg_path(session: &Session, args: &Value) -> Result<PathBuf, String> {
    arg_str(args, "file_path")
        .or_else(|| arg_str(args, "path"))
        .map(|p| resolve(&session.cwd, p))
        .ok_or_else(|| "file_path is required".into())
}

pub async fn execute(
    session: &mut Session,
    name: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> (bool, String) {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return (false, "cancelled".into());
    }
    match canonicalize(name) {
        "view" => view(session, args),
        "write" => edit::write_file(session, args),
        "edit" => edit::edit(session, args),
        "multiedit" => edit::multiedit(session, args),
        "grep" => grep(session, args),
        "glob" => glob_files(session, args),
        "ls" => ls(session, args),
        "bash" => jobs::bash(session, args, cancel).await,
        "job_output" => jobs::job_output(args, cancel).await,
        "job_kill" => jobs::job_kill(args),
        "search" => web::web_search(args).await,
        "sourcegraph" => web::sourcegraph(args).await,
        "web_fetch" => web::web_fetch(session, args).await,
        "fetch" => web::fetch(args).await,
        "download" => web::download(session, args).await,
        "lsp_replace_symbol" => symbols::lsp_replace_symbol(session, args),
        "lsp_rename" => symbols::lsp_rename(session, args),
        "lsp_definition" => symbols::lsp_definition(session, args),
        "lsp_symbols" => symbols::lsp_symbols(session, args),
        "references" => symbols::references(session, args),
        "lsp_call_hierarchy" => symbols::lsp_call_hierarchy(session, args),
        "diagnostics" => symbols::diagnostics(session, args),
        "todos" => todo_write(session, args),
        other => (false, format!("unknown tool: {other}")),
    }
}

fn view(session: &Session, args: &Value) -> (bool, String) {
    let path = match arg_path(session, args) {
        Ok(p) => p,
        Err(e) => return (false, e),
    };
    if path.is_dir() {
        return ls(session, args);
    }
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
            (true, out)
        }
        Err(e) => (false, e.to_string()),
    }
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
        if let Some(g) = glob {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !glob_match(g, name) {
                continue;
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
    let root = arg_str(args, "path")
        .map(|p| resolve(&session.cwd, p))
        .unwrap_or_else(|| session.cwd.clone());
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

fn ls(session: &Session, args: &Value) -> (bool, String) {
    let path = arg_str(args, "path")
        .or_else(|| arg_str(args, "file_path"))
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
    use serde_json::json;

    #[test]
    fn glob_starstar_matches_nested() {
        assert!(glob_match("**/*.rs", "src/app.rs"));
        assert!(glob_match("*.rs", "app.rs"));
        assert!(!glob_match("*.rs", "src/app.rs"));
    }

    #[test]
    fn edit_unique() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "alpha beta alpha").unwrap();
        let s = Session::new(dir.path().to_path_buf(), "m".into());
        let (ok, msg) = edit::edit(
            &s,
            &json!({"file_path":"a.txt","old_string":"beta","new_string":"BETA"}),
        );
        assert!(ok, "{msg}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "alpha BETA alpha");
        assert!(msg.starts_with(EDIT_DIFF), "{msg}");
    }

    #[test]
    fn edit_matches_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.rs");
        std::fs::write(&file, "fn go() {\n    let x = 1;\n}\n").unwrap();
        let s = Session::new(dir.path().to_path_buf(), "m".into());
        let (ok, msg) = edit::edit(
            &s,
            &json!({
                "file_path":"a.rs",
                "old_string":"fn go() {\n  let x = 1;\n}",
                "new_string":"fn go() {\n  let x = 2;\n}"
            }),
        );
        assert!(ok, "{msg}");
        let got = std::fs::read_to_string(&file).unwrap();
        assert!(got.contains("let x = 2"), "{got}");
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
            execute(
                &mut session,
                "bash",
                &json!({"command": "sleep 30", "auto_background_after": 120}),
                &flag,
            )
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let (ok, out) = tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .expect("bash did not stop")
            .expect("join");
        assert!(!ok, "{out}");
        assert!(out.contains("cancelled"), "{out}");
        let _ = &mut s;
    }
}
