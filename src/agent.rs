use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::ollama::Client;
use crate::session::{AgentMode, ChatMessage, Session};
use crate::tools;

/// How many model/tool rounds one turn may spend before it pauses. Agentic work
/// legitimately needs many passes (read, edit, test, read again), so the budget
/// is generous; it only exists to stop a runaway loop. Override with
/// `ZODER_MAX_ROUNDS`.
const DEFAULT_MAX_ROUNDS: usize = 200;
/// Identical tool calls in a row before the turn is called stuck.
const REPEAT_LIMIT: usize = 6;

fn max_rounds() -> usize {
    std::env::var("ZODER_MAX_ROUNDS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_ROUNDS)
}

/// Watches for the model calling the same tool with the same arguments over and
/// over, which otherwise burns the whole round budget on no progress.
#[derive(Default)]
struct RepeatGuard {
    last: Option<(String, usize)>,
}

impl RepeatGuard {
    fn tripped(&mut self, sig: &str) -> bool {
        match &mut self.last {
            Some((last, n)) if last == sig => {
                *n += 1;
                *n >= REPEAT_LIMIT
            }
            _ => {
                self.last = Some((sig.to_string(), 1));
                false
            }
        }
    }
}

pub enum AgentEvent {
    ThinkingDelta(String),
    ContentDelta(String),
    ToolStart {
        id: String,
        name: String,
        detail: String,
    },
    ToolEnd {
        id: String,
        output: String,
        ok: bool,
    },
    NeedPermission {
        id: String,
        name: String,
        detail: String,
        reply: oneshot::Sender<bool>,
    },
    Todos(Vec<crate::session::Todo>),
    SyncMessages(Vec<ChatMessage>),
    HostModels(Vec<String>),
    Status(String),
    Usage {
        prompt: u32,
        eval: u32,
    },
    Done,
    Error(String),
}

pub struct TurnInput {
    pub client: Client,
    pub messages: Vec<ChatMessage>,
    pub mode: AgentMode,
    pub cwd: std::path::PathBuf,
    pub session_dir: std::path::PathBuf,
    pub plan_path: std::path::PathBuf,
    pub always: bool,
    pub cancel: Arc<AtomicBool>,
}

pub fn spawn(
    input: TurnInput,
    tx: mpsc::UnboundedSender<AgentEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        match run_turn(input, tx.clone()).await {
            Ok(messages) => {
                let _ = tx.send(AgentEvent::SyncMessages(messages));
            }
            Err(e) => {
                let _ = tx.send(AgentEvent::Error(e.to_string()));
            }
        }
        let _ = tx.send(AgentEvent::Done);
    })
}

async fn run_turn(
    mut input: TurnInput,
    tx: mpsc::UnboundedSender<AgentEvent>,
) -> anyhow::Result<Vec<ChatMessage>> {
    let tools_spec = tools::definitions();
    let mut guard = Session::new(input.cwd.clone(), input.client.model.clone());
    guard.mode = input.mode;

    let rounds = max_rounds();
    let mut repeat = RepeatGuard::default();
    for round in 0..rounds {
        if input.cancel.load(Ordering::Relaxed) {
            let _ = tx.send(AgentEvent::Status("cancelled".into()));
            return Ok(input.messages);
        }
        let tx2 = tx.clone();
        let result = input
            .client
            .stream_chat(&input.messages, &tools_spec, true, |chunk| {
                if !chunk.message.thinking.is_empty() {
                    let _ = tx2.send(AgentEvent::ThinkingDelta(chunk.message.thinking.clone()));
                }
                if !chunk.message.content.is_empty() {
                    let _ = tx2.send(AgentEvent::ContentDelta(chunk.message.content.clone()));
                }
            })
            .await;

        let (content, thinking, calls, prompt, eval) = match result {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.send(AgentEvent::Error(e.to_string()));
                return Ok(input.messages);
            }
        };
        let _ = tx.send(AgentEvent::Usage { prompt, eval });

        let tool_val = if calls.is_empty() {
            None
        } else {
            serde_json::to_value(&calls).ok()
        };
        input.messages.push(ChatMessage::assistant(
            content,
            if thinking.is_empty() {
                None
            } else {
                Some(thinking)
            },
            tool_val,
        ));

        if calls.is_empty() {
            return Ok(input.messages);
        }

        for (i, call) in calls.iter().enumerate() {
            if input.cancel.load(Ordering::Relaxed) {
                return Ok(input.messages);
            }
            if let Some(stop) =
                dispatch_call(&mut input, &mut guard, &tx, call, round, i, &mut repeat).await
            {
                return Ok(stop);
            }
        }
        let _ = tx.send(AgentEvent::SyncMessages(input.messages.clone()));
    }
    let _ = tx.send(AgentEvent::Status(format!(
        "paused after {rounds} tool rounds — send a message to carry on"
    )));
    Ok(input.messages)
}

fn reject_tool(
    messages: &mut Vec<ChatMessage>,
    tx: &mpsc::UnboundedSender<AgentEvent>,
    name: &str,
    id: String,
    msg: String,
) {
    messages.push(ChatMessage::tool(name, &msg));
    let _ = tx.send(AgentEvent::ToolEnd {
        id,
        output: msg,
        ok: false,
    });
}

async fn dispatch_call(
    input: &mut TurnInput,
    guard: &mut Session,
    tx: &mpsc::UnboundedSender<AgentEvent>,
    call: &crate::ollama::ToolCall,
    round: usize,
    i: usize,
    repeat: &mut RepeatGuard,
) -> Option<Vec<ChatMessage>> {
    let name = call.function.name.clone();
    let args = normalize_args(&call.function.arguments);
    if repeat.tripped(&format!("{name} {args}")) {
        let msg = format!(
            "{name} was called {REPEAT_LIMIT} times in a row with the same arguments — pausing instead of spinning"
        );
        input.messages.push(ChatMessage::tool(&name, &msg));
        let _ = tx.send(AgentEvent::Status(msg));
        return Some(std::mem::take(&mut input.messages));
    }
    let detail = tools::detail(&name, &args);
    let id = format!("r{round}-{i}-{name}");
    let _ = tx.send(AgentEvent::ToolStart {
        id: id.clone(),
        name: name.clone(),
        detail: detail.clone(),
    });

    if let Some(msg) = tools::plan_forbidden(input.mode, &name, &args, &input.plan_path) {
        reject_tool(&mut input.messages, tx, &name, id, msg);
        return None;
    }

    let mut allow =
        input.always || input.mode == AgentMode::Always || !tools::needs_permission(&name);
    if !allow {
        let (rtx, rrx) = oneshot::channel();
        let _ = tx.send(AgentEvent::NeedPermission {
            id: id.clone(),
            name: name.clone(),
            detail: detail.clone(),
            reply: rtx,
        });
        allow = rrx.await.unwrap_or(false);
    }
    if !allow {
        reject_tool(&mut input.messages, tx, &name, id, "denied by user".into());
        return None;
    }

    if name == "write" || name == "search_replace" {
        if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
            if p.ends_with("plan.md") {
                std::fs::create_dir_all(&input.session_dir).ok();
            }
        }
    }

    let (ok, output) = tools::execute(guard, &name, &args).await;
    if name == "todo_write" {
        let _ = tx.send(AgentEvent::Todos(guard.todos.clone()));
    }
    input.messages.push(ChatMessage::tool(&name, &output));
    let _ = tx.send(AgentEvent::ToolEnd { id, output, ok });
    None
}

fn normalize_args(v: &Value) -> Value {
    if let Some(s) = v.as_str() {
        serde_json::from_str(s).unwrap_or_else(|_| serde_json::json!({}))
    } else if v.is_object() {
        v.clone()
    } else {
        serde_json::json!({})
    }
}

pub fn system_prompt(
    cwd: &std::path::Path,
    mode: AgentMode,
    plan_path: &std::path::Path,
) -> String {
    let mut s = format!(
        "You are Zoder, a terminal coding assistant. You work inside a full-screen TUI.\n\
         Workspace: {}\n\
         Be concrete. Prefer existing patterns in this repo. Do not invent files that do not exist — read them first.\n\
         Use tools to inspect and change the workspace. When editing, keep diffs small.\n\
         Reply in the user's language. Never print secrets.\n\
         After finishing, give a short summary of what changed.\n",
        cwd.display()
    );
    if mode == AgentMode::Plan {
        s.push_str(&format!(
            "\nPLAN MODE is on. Do not modify any file except {}.\n\
             Explore with read/grep/glob/list_dir, then write a plan to that file with:\n\
             - Context\n- Approach\n- Files to change\n- Reuse (existing functions)\n- Verification\n\
             Do not implement until the user approves.\n",
            plan_path.display()
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_budget_is_generous_and_overridable() {
        // One test owns the env var so parallel tests cannot race on it.
        std::env::remove_var("ZODER_MAX_ROUNDS");
        assert!(
            max_rounds() >= 100,
            "a turn should survive a long edit/test cycle"
        );
        std::env::set_var("ZODER_MAX_ROUNDS", "  ");
        assert_eq!(max_rounds(), DEFAULT_MAX_ROUNDS);
        std::env::set_var("ZODER_MAX_ROUNDS", "7");
        assert_eq!(max_rounds(), 7);
        std::env::remove_var("ZODER_MAX_ROUNDS");
    }

    #[test]
    fn repeat_guard_trips_only_on_an_identical_run() {
        let mut g = RepeatGuard::default();
        for _ in 0..REPEAT_LIMIT - 1 {
            assert!(!g.tripped("read_file {\"path\":\"src/lib.rs\"}"));
        }
        assert!(g.tripped("read_file {\"path\":\"src/lib.rs\"}"));
        assert!(
            !g.tripped("read_file {\"path\":\"src/main.rs\"}"),
            "a different call resets the run"
        );
    }
}
