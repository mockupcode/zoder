use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::agent::{self, AgentEvent, TurnInput};
use crate::composer::DraftKind;
use crate::session::{AgentMode, Block, Session, SessionMeta, ToolStatus};
use crate::tools;

use super::{clock, App, Focus, Overlay, Screen, SessionsOverlay};

impl App {
    pub(super) fn submit(&mut self) {
        if let Some(cmds) = self.slash_items() {
            if !cmds.is_empty() {
                let name = cmds[self.slash_sel.min(cmds.len() - 1)].name;
                let rest = self
                    .composer
                    .text
                    .split_once(' ')
                    .map(|(_, r)| r.to_string())
                    .unwrap_or_default();
                self.composer.clear();
                self.run_slash(name, rest.trim());
                return;
            }
        }
        if self.confirm_at(true) {
            return;
        }

        if self.composer.kind == DraftKind::Shell {
            let cmd = self.composer.take();
            if cmd.trim().is_empty() {
                return;
            }
            self.screen = Screen::Chat;
            let shown = format!("! {cmd}");
            self.session.blocks.push(Block::User {
                text: shown.clone(),
                time: clock(),
            });
            self.session
                .messages
                .push(crate::session::ChatMessage::user(&shown));
            self.spawn_shell(cmd);
            return;
        }

        let text = self.composer.take();
        if text.trim().is_empty() {
            return;
        }
        self.send_user(text);
    }

    pub(super) fn spawn_shell(&mut self, cmd: String) {
        let cwd = self.session.cwd.clone();
        let tx = self.tx.clone();
        self.begin_turn();
        let turn = self.live_turn;
        let cancel = self.cancel.clone();
        if tokio::runtime::Handle::try_current().is_ok() {
            self.turn_task = Some(tokio::spawn(async move {
                let mut s = Session::new(cwd, String::new());
                let id = "shell".to_string();
                let _ = tx.send((
                    turn,
                    AgentEvent::ToolStart {
                        id: id.clone(),
                        name: "bash".into(),
                        detail: cmd.clone(),
                    },
                ));
                let args = serde_json::json!({"command": cmd});
                let (ok, output) = tools::execute(&mut s, "bash", &args, &cancel).await;
                let _ = tx.send((turn, AgentEvent::ToolEnd { id, output, ok }));
                let _ = tx.send((turn, AgentEvent::Done));
            }));
        }
    }

    pub fn send_user(&mut self, text: String) {
        self.touch();
        self.screen = Screen::Chat;
        if self.running {
            self.queue.push(text);
            self.toast("queued until this turn finishes");
            return;
        }
        self.session.maybe_title_from(&text);
        self.session.blocks.push(Block::User {
            text: text.clone(),
            time: clock(),
        });
        let sys = agent::system_prompt(
            &self.session.cwd,
            self.session.mode,
            &self.session.plan_path(),
        );
        if self.session.messages.is_empty() {
            self.session
                .messages
                .push(crate::session::ChatMessage::system(sys));
        } else if let Some(first) = self.session.messages.first_mut() {
            if first.role == "system" {
                first.content = Some(sys);
            }
        }
        self.session
            .messages
            .push(crate::session::ChatMessage::user(&text));
        self.follow = true;
        self.scroll.set(0);
        self.start_turn();
        let _ = self.session.save();
    }

    pub(super) fn start_turn(&mut self) {
        self.begin_turn();
        self.thinking_started = Some(Instant::now());
        let input = TurnInput {
            client: self.client.clone(),
            messages: self.session.messages.clone(),
            mode: self.session.mode,
            cwd: self.session.cwd.clone(),
            session_dir: self.session.dir(),
            plan_path: self.session.plan_path(),
            always: self.session.mode == AgentMode::Always,
            cancel: self.cancel.clone(),
            turn: self.live_turn,
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            self.turn_task = Some(agent::spawn(input, self.tx.clone()));
        }
    }

    fn begin_turn(&mut self) {
        if let Some(h) = self.turn_task.take() {
            h.abort();
        }
        self.live_turn = self.live_turn.wrapping_add(1);
        self.cancel.store(false, Ordering::Relaxed);
        self.running = true;
    }

    pub(super) fn cancel_turn(&mut self) {
        if !self.running {
            return;
        }
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(h) = self.turn_task.take() {
            h.abort();
        }
        self.live_turn = self.live_turn.wrapping_add(1);
        self.running = false;
        self.thinking_started = None;
        self.answer_perm(false);
        for b in &mut self.session.blocks {
            match b {
                Block::Tool {
                    status,
                    output,
                    folded,
                    ..
                } if *status == ToolStatus::Running => {
                    *status = ToolStatus::Failed;
                    *output = "cancelled".into();
                    *folded = true;
                }
                Block::Thinking { folded, .. } => *folded = true,
                _ => {}
            }
        }
        self.session.blocks.push(Block::Notice {
            text: "cancelled".into(),
        });
        let _ = self.session.save();
        self.toast("cancelled");
    }

    pub(super) fn run_slash(&mut self, name: &str, rest: &str) {
        match name {
            "new" | "clear" => self.new_session(),
            "resume" => self.open_sessions(),
            "home" => {
                self.screen = Screen::Welcome;
                self.focus = Focus::Prompt;
            }
            "plan" => {
                self.session.mode = AgentMode::Plan;
                if rest.is_empty() {
                    self.toast("plan mode — send a task");
                } else {
                    self.send_user(rest.to_string());
                }
            }
            "view-plan" => {
                self.show_todos = true;
                let p = self.session.plan_path();
                if let Ok(s) = std::fs::read_to_string(&p) {
                    self.session.blocks.push(Block::Notice { text: s });
                } else {
                    self.toast("no plan written yet");
                }
            }
            "always-approve" => self.cycle_always(),
            "status" => {
                let n = self
                    .session
                    .blocks
                    .iter()
                    .filter(|b| matches!(b, Block::User { .. }))
                    .count();
                self.session.blocks.push(Block::Notice {
                    text: format!(
                        "cwd {}\nhost {}\nmodel {}\nmode {}\nturns {n}\ntokens {}/{}",
                        self.session.cwd.display(),
                        self.cfg.host(),
                        self.session.model,
                        self.session.mode.label(),
                        self.session.prompt_tokens,
                        self.session.eval_tokens
                    ),
                });
                self.screen = Screen::Chat;
            }
            "model" => {
                if rest.is_empty() {
                    self.open_models();
                } else {
                    self.apply_model(rest.to_string());
                    self.toast(format!("model {rest}"));
                }
            }
            "copy" => {
                if let Some(Block::Assistant { text, .. }) = self
                    .session
                    .blocks
                    .iter()
                    .rev()
                    .find(|b| matches!(b, Block::Assistant { .. }))
                {
                    self.toast(format!(
                        "{} chars in last reply (clipboard not wired; see transcript)",
                        text.len()
                    ));
                }
            }
            "help" => self.open_help(),
            "quit" | "exit" => self.should_quit = true,
            _ => self.toast(format!("unknown command /{name}")),
        }
    }

    pub(super) fn new_session(&mut self) {
        if self.running {
            self.cancel.store(true, Ordering::Relaxed);
            if let Some(h) = self.turn_task.take() {
                h.abort();
            }
            self.live_turn = self.live_turn.wrapping_add(1);
        }
        let _ = self.session.save();
        let cwd = self.session.cwd.clone();
        self.session = Session::new(cwd.clone(), self.cfg.model().to_string());
        self.screen = Screen::Welcome;
        self.composer.clear();
        self.scroll.set(0);
        self.follow = true;
        self.running = false;
        self.queue.clear();
        self.sessions = Session::list(&cwd);
        self.toast("new session");
    }

    pub(super) fn open_sessions(&mut self) {
        self.sessions = Session::list(&self.session.cwd);
        self.overlay = Overlay::Sessions(SessionsOverlay::open());
    }

    pub(super) fn open_help(&mut self) {
        self.overlay = Overlay::Help;
    }

    pub fn picker_sessions(&self) -> Vec<&SessionMeta> {
        let Some(s) = self.overlay.sessions() else {
            return self.sessions.iter().collect();
        };
        Session::grouped(self.filtered_sessions(&s.query, s.filter_cwd), s.expanded)
    }

    fn picker_owned(&self) -> Vec<SessionMeta> {
        self.picker_sessions().into_iter().cloned().collect()
    }

    pub(super) fn resume_selected(&mut self, selected: usize) {
        let list = self.picker_owned();
        self.overlay = Overlay::None;
        if let Some(meta) = list.get(selected) {
            if let Ok(s) = Session::load(&meta.path) {
                self.session = s;
                self.screen = Screen::Chat;
                self.follow = true;
                self.toast("resumed");
            }
        }
    }

    pub(super) fn delete_selected(&mut self, selected: usize) {
        let list = self.picker_owned();
        let Some(meta) = list.get(selected).cloned() else {
            return;
        };
        let current = self.session.id == meta.id;
        if Session::delete(&meta.path).is_ok() {
            self.sessions = Session::list(&self.session.cwd);
            if current {
                self.overlay = Overlay::None;
                self.new_session();
                self.toast("session deleted");
                return;
            }
            self.toast("session deleted");
        }
        if let Some(s) = self.overlay.sessions_mut() {
            s.confirm_delete = false;
            s.searching = false;
        }
        let n = self.picker_sessions().len();
        if let Some(s) = self.overlay.sessions_mut() {
            s.selected = selected.min(n.saturating_sub(1));
        }
    }

    pub(super) fn apply_model(&mut self, m: String) {
        self.cfg.set_model(m.clone());
        self.client.model = m.clone();
        self.session.model = m;
    }

    pub(super) fn open_models(&mut self) {
        let items = match &self.connected {
            Some(Ok(v)) => v.clone(),
            _ => vec![self.cfg.model().to_string()],
        };
        self.overlay = Overlay::Models { items, selected: 0 };
    }

    pub fn on_agent(&mut self, turn: u64, ev: AgentEvent) {
        if turn != 0 && turn != self.live_turn {
            if let AgentEvent::NeedPermission { reply, .. } = ev {
                let _ = reply.send(false);
            }
            return;
        }
        self.touch();
        match ev {
            AgentEvent::ThinkingDelta(s) => match self.session.blocks.last_mut() {
                Some(Block::Thinking { text, .. }) => text.push_str(&s),
                _ => self.session.blocks.push(Block::Thinking {
                    text: s,
                    folded: false,
                    ms: 0,
                }),
            },
            AgentEvent::ContentDelta(s) => {
                self.fold_thinking(
                    self.thinking_started
                        .map(|t0| t0.elapsed().as_millis() as u64),
                );
                match self.session.blocks.last_mut() {
                    Some(Block::Assistant { text, .. }) => text.push_str(&s),
                    _ => self.session.blocks.push(Block::Assistant {
                        text: s,
                        time: clock(),
                    }),
                }
            }
            AgentEvent::ToolStart { id, name, detail } => {
                self.fold_thinking(None);
                self.session.blocks.push(Block::Tool {
                    id,
                    name,
                    detail,
                    output: String::new(),
                    status: ToolStatus::Running,
                    folded: false,
                    elapsed_ms: 0,
                });
            }
            AgentEvent::ToolEnd { id, output, ok } => {
                if let Some(Block::Tool {
                    output: o,
                    status,
                    folded,
                    ..
                }) = self.session.last_mut_tool(&id)
                {
                    *o = output;
                    *status = if ok {
                        ToolStatus::Ok
                    } else {
                        ToolStatus::Failed
                    };
                    *folded = o.lines().count() > 12;
                }
            }
            AgentEvent::NeedPermission {
                name,
                detail,
                reply,
                ..
            } => {
                self.overlay = Overlay::Permission {
                    name,
                    detail,
                    selected: 0,
                    reply: Some(reply),
                };
            }
            AgentEvent::Todos(t) => {
                self.session.todos = t;
                self.show_todos = true;
            }
            AgentEvent::SyncMessages(m) => {
                if !m.is_empty() {
                    self.session.messages = m;
                }
            }
            AgentEvent::Status(s) => {
                self.status_line = s.clone();
                self.session.blocks.push(Block::Notice { text: s });
            }
            AgentEvent::Usage { prompt, eval } => {
                self.session.prompt_tokens = prompt;
                self.session.eval_tokens = eval;
            }
            AgentEvent::Error(e) => {
                if self.screen == Screen::Welcome {
                    self.connected = Some(Err(e.clone()));
                    self.toast(e);
                } else {
                    self.session.blocks.push(Block::Error { text: e });
                }
            }
            AgentEvent::HostModels(models) => {
                self.connected = Some(Ok(models));
            }
            AgentEvent::Done => {
                self.running = false;
                if let Some(t0) = self.thinking_started.take() {
                    let secs = t0.elapsed().as_secs();
                    if secs > 0 {
                        self.session.blocks.push(Block::Notice {
                            text: format!("Worked for {secs}s"),
                        });
                    }
                }
                let _ = self.session.save();
                if !self.queue.is_empty() {
                    let next = self.queue.remove(0);
                    self.send_user(next);
                }
            }
        }
        if self.follow {
            self.scroll.set(0);
            self.selected_block = self.session.blocks.len().saturating_sub(1);
        }
    }

    fn fold_thinking(&mut self, ms: Option<u64>) {
        if let Some(Block::Thinking {
            folded, ms: slot, ..
        }) = self.session.blocks.last_mut()
        {
            *folded = true;
            if let Some(v) = ms {
                *slot = v;
            }
        }
    }

    pub fn filtered_sessions(&self, query: &str, filter_cwd: bool) -> Vec<&SessionMeta> {
        let q = query.to_lowercase();
        self.sessions
            .iter()
            .filter(|s| {
                let hit = q.is_empty() || s.title.to_lowercase().contains(&q) || s.id.contains(&q);
                let folder = !filter_cwd || s.cwd == self.session.cwd;
                hit && folder
            })
            .collect()
    }
}
