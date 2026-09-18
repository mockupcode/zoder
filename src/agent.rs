use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::ollama::Client;
use crate::session::{AgentMode, ChatMessage, Session};
use crate::tools;

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
    let mut guard = Session {
        id: "live".into(),
        title: String::new(),
        created: chrono::Local::now(),
        cwd: input.cwd.clone(),
        model: input.client.model.clone(),
        mode: input.mode,
        blocks: Vec::new(),
        messages: Vec::new(),
        todos: Vec::new(),
        prompt_tokens: 0,
        eval_tokens: 0,
        updated: chrono::Local::now(),
    };

    for round in 0..24 {
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
            let name = call.function.name.clone();
            let args = normalize_args(&call.function.arguments);
            let detail = tools::detail(&name, &args);
            let id = format!("r{round}-{i}-{name}");
            let _ = tx.send(AgentEvent::ToolStart {
                id: id.clone(),
                name: name.clone(),
                detail: detail.clone(),
            });

            if let Some(msg) = tools::plan_forbidden(input.mode, &name, &args, &input.plan_path) {
                input.messages.push(ChatMessage::tool(&name, &msg));
                let _ = tx.send(AgentEvent::ToolEnd {
                    id,
                    output: msg,
                    ok: false,
                });
                continue;
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
                let msg = "denied by user";
                input.messages.push(ChatMessage::tool(&name, msg));
                let _ = tx.send(AgentEvent::ToolEnd {
                    id,
                    output: msg.into(),
                    ok: false,
                });
                continue;
            }

            if name == "write" || name == "search_replace" {
                if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                    if p.ends_with("plan.md") {
                        std::fs::create_dir_all(&input.session_dir).ok();
                    }
                }
            }

            let (ok, output) = tools::execute(&mut guard, &name, &args).await;
            if name == "todo_write" {
                let _ = tx.send(AgentEvent::Todos(guard.todos.clone()));
            }
            input.messages.push(ChatMessage::tool(&name, &output));
            let _ = tx.send(AgentEvent::ToolEnd { id, output, ok });
        }
        let _ = tx.send(AgentEvent::SyncMessages(input.messages.clone()));
    }
    let _ = tx.send(AgentEvent::Status(
        "stopped after too many tool rounds".into(),
    ));
    Ok(input.messages)
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
