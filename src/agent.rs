use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::ollama::Client;
use crate::session::{ChatMessage, Session, Todo};
use crate::tools;

/// How many model/tool rounds one turn may spend before it pauses. Agentic work
/// legitimately needs many passes (read, edit, test, read again), so the budget
/// is generous; it only exists to stop a runaway loop. Override with
/// `ZODER_MAX_ROUNDS`.
const DEFAULT_MAX_ROUNDS: usize = 200;
/// Identical tool calls in a row before the turn is called stuck.
const REPEAT_LIMIT: usize = 6;
const LARGE_WINDOW: u32 = 200_000;
const LARGE_REMAINING: u32 = 20_000;

const SUMMARY_SYSTEM: &str = "\
You are summarizing a conversation so work can continue later.

This summary will be the ONLY prior context when the conversation resumes. \
Assume all previous messages will be lost. Be thorough.

Required sections:

## Current State
- Exact user request
- What is done, what is in progress, what is left (specific next steps)

## Files & Changes
- Files modified, with a brief description
- Files read and why they matter
- Paths and line numbers for important locations

## Technical Context
- Decisions and why
- Patterns, libraries, commands that worked or failed
- Environment details

## Strategy
- Approach, alternatives rejected, gotchas, blockers

## Exact Next Steps
Numbered, specific (file + location + command), not vague.

Write as a briefing for a teammate taking over mid-task. No emojis. \
Err on too much detail.
";

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

#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub provider: String,
    pub model: String,
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
    NeedQuestion {
        prompt: String,
        hint: String,
        options: Vec<String>,
        reply: oneshot::Sender<String>,
    },
    Todos(Vec<crate::session::Todo>),
    SyncMessages(Vec<ChatMessage>),
    HostModels(Vec<String>),
    ModelCatalog(Vec<ModelEntry>),
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
    pub cwd: std::path::PathBuf,
    pub cancel: Arc<AtomicBool>,
    pub turn: u64,
    pub context_window: u32,
    pub todos: Vec<Todo>,
}

pub type Bus = mpsc::UnboundedSender<(u64, AgentEvent)>;

fn emit(tx: &Bus, turn: u64, ev: AgentEvent) {
    let _ = tx.send((turn, ev));
}

pub fn spawn(input: TurnInput, tx: Bus) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let turn = input.turn;
        match run_turn(input, tx.clone()).await {
            Ok(messages) => {
                emit(&tx, turn, AgentEvent::SyncMessages(messages));
            }
            Err(e) => {
                emit(&tx, turn, AgentEvent::Error(e.to_string()));
            }
        }
        emit(&tx, turn, AgentEvent::Done);
    })
}

async fn run_turn(mut input: TurnInput, tx: Bus) -> anyhow::Result<Vec<ChatMessage>> {
    let live = input.client.live_context_window().await;
    if live > 0 {
        input.context_window = live;
    }
    let tools_spec = tools::definitions();
    let mut guard = Session::new(input.cwd.clone(), input.client.model.clone());
    let turn = input.turn;

    let rounds = max_rounds();
    let mut repeat = RepeatGuard::default();
    for round in 0..rounds {
        if input.cancel.load(Ordering::Relaxed) {
            emit(&tx, turn, AgentEvent::Status("cancelled".into()));
            return Ok(input.messages);
        }
        let tx2 = tx.clone();
        let wired = wire_messages(&input.messages);
        let result = input
            .client
            .stream_chat(&wired, &tools_spec, true, &input.cancel, |chunk| {
                if !chunk.message.thinking.is_empty() {
                    emit(
                        &tx2,
                        turn,
                        AgentEvent::ThinkingDelta(chunk.message.thinking.clone()),
                    );
                }
                if !chunk.message.content.is_empty() {
                    emit(
                        &tx2,
                        turn,
                        AgentEvent::ContentDelta(chunk.message.content.clone()),
                    );
                }
            })
            .await;

        if input.cancel.load(Ordering::Relaxed) {
            emit(&tx, turn, AgentEvent::Status("cancelled".into()));
            return Ok(input.messages);
        }

        let (content, thinking, calls, prompt, eval) = match result {
            Ok(v) => v,
            Err(e) => {
                emit(&tx, turn, AgentEvent::Error(e.to_string()));
                return Ok(input.messages);
            }
        };
        emit(&tx, turn, AgentEvent::Usage { prompt, eval });

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
            maybe_compact(&mut input, &tx, prompt.saturating_add(eval)).await;
            return Ok(input.messages);
        }

        for (i, call) in calls.iter().enumerate() {
            if input.cancel.load(Ordering::Relaxed) {
                emit(&tx, turn, AgentEvent::Status("cancelled".into()));
                return Ok(input.messages);
            }
            if let Some(stop) =
                dispatch_call(&mut input, &mut guard, &tx, call, round, i, &mut repeat).await
            {
                return Ok(stop);
            }
        }
        emit(&tx, turn, AgentEvent::SyncMessages(input.messages.clone()));
        maybe_compact(&mut input, &tx, prompt.saturating_add(eval)).await;
    }
    emit(
        &tx,
        turn,
        AgentEvent::Status(format!(
            "paused after {rounds} tool rounds — send a message to carry on"
        )),
    );
    Ok(input.messages)
}

async fn dispatch_call(
    input: &mut TurnInput,
    guard: &mut Session,
    tx: &Bus,
    call: &crate::ollama::ToolCall,
    round: usize,
    i: usize,
    repeat: &mut RepeatGuard,
) -> Option<Vec<ChatMessage>> {
    let turn = input.turn;
    let name = call.function.name.clone();
    let args = normalize_args(&call.function.arguments);
    if repeat.tripped(&format!("{name} {args}")) {
        let msg = format!(
            "{name} was called {REPEAT_LIMIT} times in a row with the same arguments — pausing instead of spinning"
        );
        input.messages.push(ChatMessage::tool(&name, &msg));
        emit(tx, turn, AgentEvent::Status(msg));
        return Some(std::mem::take(&mut input.messages));
    }
    let detail = tools::detail(&name, &args);
    let id = format!("r{round}-{i}-{name}");
    emit(
        tx,
        turn,
        AgentEvent::ToolStart {
            id: id.clone(),
            name: name.clone(),
            detail: detail.clone(),
        },
    );

    if input.cancel.load(Ordering::Relaxed) {
        emit(tx, turn, AgentEvent::Status("cancelled".into()));
        return Some(std::mem::take(&mut input.messages));
    }

    if tools::canonicalize(&name) == "question" {
        let (ok, output) = ask_questions(&input.cancel, tx, turn, &args).await;
        input.messages.push(ChatMessage::tool(&name, &output));
        emit(tx, turn, AgentEvent::ToolEnd { id, output, ok });
        return None;
    }

    let (ok, output) = tools::execute(guard, &name, &args, &input.cancel).await;
    if tools::canonicalize(&name) == "todos" {
        emit(tx, turn, AgentEvent::Todos(guard.todos.clone()));
    }
    input.messages.push(ChatMessage::tool(&name, &output));
    emit(tx, turn, AgentEvent::ToolEnd { id, output, ok });
    None
}

async fn wait_cancel(flag: &AtomicBool) {
    loop {
        if flag.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }
}

async fn ask_questions(cancel: &AtomicBool, tx: &Bus, turn: u64, args: &Value) -> (bool, String) {
    let Some(qs) = args.get("questions").and_then(|v| v.as_array()) else {
        return (false, "at least one question is required".into());
    };
    if qs.is_empty() {
        return (false, "at least one question is required".into());
    }
    if qs.len() > 5 {
        return (false, "exceeds maximum of 5 questions per batch".into());
    }
    let mut answers = Vec::new();
    for (i, q) in qs.iter().enumerate() {
        let qtype = q
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("free_text");
        let prompt = q
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let hint = q
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if hint.is_empty() {
            return (false, format!("question {} is missing description", i + 1));
        }
        let mut options = Vec::new();
        match qtype {
            "yes_no" => {
                options = vec!["yes".into(), "no".into()];
            }
            "single_choice" | "multi_choice" => {
                let ch = q
                    .get("choices")
                    .or_else(|| q.get("options"))
                    .and_then(|v| v.as_array());
                if let Some(ch) = ch {
                    for c in ch.iter().take(5) {
                        let label = c
                            .get("label")
                            .or_else(|| c.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        options.push(label.to_string());
                    }
                }
                if options.len() < 2 && qtype != "free_text" {
                    return (
                        false,
                        format!("question {} needs at least 2 choices", i + 1),
                    );
                }
            }
            _ => {}
        }
        let (rtx, rrx) = oneshot::channel();
        emit(
            tx,
            turn,
            AgentEvent::NeedQuestion {
                prompt: prompt.clone(),
                hint,
                options,
                reply: rtx,
            },
        );
        let ans = tokio::select! {
            r = rrx => r.unwrap_or_else(|_| "cancelled".into()),
            _ = wait_cancel(cancel) => "cancelled".into(),
        };
        if ans == "cancelled" {
            return (false, "User cancelled this question".into());
        }
        answers.push(format!("Q{}: {prompt}\nUser answered: {ans}", i + 1));
    }
    (true, answers.join("\n\n"))
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

pub fn should_compact(used: u32, window: u32) -> bool {
    if window == 0 || used == 0 {
        return false;
    }
    let remaining = window.saturating_sub(used);
    let threshold = if window > LARGE_WINDOW {
        LARGE_REMAINING
    } else {
        ((window as f64) * 0.2).round() as u32
    };
    remaining <= threshold
}

/// History actually sent to the model: last compact summary plus later turns.
pub fn wire_messages(msgs: &[ChatMessage]) -> Vec<ChatMessage> {
    let sys = msgs.iter().find(|m| m.role == "system").cloned();
    let Some(i) = msgs.iter().rposition(|m| m.is_summary) else {
        return msgs.to_vec();
    };
    let mut out = Vec::new();
    if let Some(s) = sys {
        out.push(s);
    }
    let mut sum = msgs[i].clone();
    sum.role = "user".into();
    sum.is_summary = false;
    sum.thinking = None;
    sum.tool_calls = None;
    if let Some(body) = sum.content.take() {
        sum.content = Some(format!("Conversation summary (prior context):\n{body}"));
    }
    out.push(sum);
    out.extend(msgs[i + 1..].iter().filter(|m| m.role != "system").cloned());
    out
}

fn summary_user_prompt(todos: &[Todo]) -> String {
    let mut s = String::from("Provide a detailed summary of our conversation above.");
    if todos.is_empty() {
        return s;
    }
    s.push_str("\n\n## Current Todo List\n\n");
    for t in todos {
        s.push_str(&format!("- [{}] {}\n", t.status, t.content));
    }
    s.push_str(
        "\nInclude these tasks and their statuses. The resuming assistant should keep using the todos tool.",
    );
    s
}

pub async fn compact_messages(
    client: &Client,
    mut messages: Vec<ChatMessage>,
    todos: &[Todo],
    cancel: &AtomicBool,
) -> anyhow::Result<Vec<ChatMessage>> {
    let mut hist: Vec<ChatMessage> = wire_messages(&messages)
        .into_iter()
        .filter(|m| m.role != "system")
        .collect();
    hist.insert(0, ChatMessage::system(SUMMARY_SYSTEM));
    hist.push(ChatMessage::user(summary_user_prompt(todos)));
    let (content, _, _, _, _) = client
        .stream_chat(&hist, &Value::Array(Vec::new()), false, cancel, |_| {})
        .await?;
    if content.trim().is_empty() {
        anyhow::bail!("compact produced no summary");
    }
    messages.push(ChatMessage::summary(content));
    Ok(messages)
}

async fn maybe_compact(input: &mut TurnInput, tx: &Bus, used: u32) {
    if !should_compact(used, input.context_window) {
        return;
    }
    if input.cancel.load(Ordering::Relaxed) {
        return;
    }
    emit(
        tx,
        input.turn,
        AgentEvent::Status("compacting context".into()),
    );
    match compact_messages(
        &input.client,
        input.messages.clone(),
        &input.todos,
        &input.cancel,
    )
    .await
    {
        Ok(msgs) => {
            input.messages = msgs;
            emit(
                tx,
                input.turn,
                AgentEvent::Status("context compacted".into()),
            );
            emit(
                tx,
                input.turn,
                AgentEvent::SyncMessages(input.messages.clone()),
            );
        }
        Err(e) => {
            emit(
                tx,
                input.turn,
                AgentEvent::Status(format!("compact failed: {e}")),
            );
        }
    }
}

pub fn system_prompt(cwd: &std::path::Path) -> String {
    format!(
        "You are Zoder, a terminal coding assistant. You work inside a full-screen TUI.\n\
         Workspace: {}\n\
         Be concrete. Prefer existing patterns in this repo. Do not invent files that do not exist — read them first.\n\
         Use tools to inspect and change the workspace. When editing, keep diffs small.\n\
         Reply in the user's language. Never print secrets.\n\
         After finishing, give a short summary of what changed.\n",
        cwd.display()
    )
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
            assert!(!g.tripped("view {\"file_path\":\"src/lib.rs\"}"));
        }
        assert!(g.tripped("view {\"file_path\":\"src/lib.rs\"}"));
        assert!(
            !g.tripped("view {\"file_path\":\"src/main.rs\"}"),
            "a different call resets the run"
        );
    }

    #[test]
    fn compact_skips_unknown_window() {
        assert!(!should_compact(50_000, 0));
        assert!(should_compact(110_000, 128_000));
        assert!(!should_compact(10_000, 128_000));
        assert!(should_compact(240_000, 256_000));
    }

    #[test]
    fn wire_messages_starts_at_last_summary() {
        let msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("old"),
            ChatMessage::assistant("a".into(), None, None),
            ChatMessage::summary("keep this"),
            ChatMessage::user("new"),
        ];
        let w = wire_messages(&msgs);
        assert_eq!(w[0].role, "system");
        assert_eq!(w[1].role, "user");
        assert!(w[1].content.as_deref().unwrap_or("").contains("keep this"));
        assert_eq!(w[2].content.as_deref(), Some("new"));
        assert_eq!(w.len(), 3);
    }
}
