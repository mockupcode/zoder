use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentMode {
    Normal,
    Plan,
    Always,
}

impl AgentMode {
    pub fn next(self) -> Self {
        match self {
            Self::Normal => Self::Plan,
            Self::Plan => Self::Always,
            Self::Always => Self::Normal,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Plan => "Plan",
            Self::Always => "Always",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    Running,
    Ok,
    Denied,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Block {
    User {
        text: String,
        #[serde(default)]
        time: String,
    },
    Assistant {
        text: String,
        #[serde(default)]
        time: String,
    },
    Thinking {
        text: String,
        folded: bool,
        ms: u64,
    },
    Tool {
        id: String,
        name: String,
        detail: String,
        output: String,
        status: ToolStatus,
        folded: bool,
        elapsed_ms: u64,
    },
    Notice {
        text: String,
    },
    Error {
        text: String,
    },
}

impl Block {
    pub fn fold_toggle(&mut self) {
        match self {
            Block::Thinking { folded, .. } | Block::Tool { folded, .. } => *folded = !*folded,
            _ => {}
        }
    }

    pub fn set_folded(&mut self, v: bool) {
        match self {
            Block::Thinking { folded, .. } | Block::Tool { folded, .. } => *folded = v,
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl ChatMessage {
    pub fn system(s: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(s.into()),
            thinking: None,
            tool_calls: None,
            tool_name: None,
        }
    }

    pub fn user(s: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(s.into()),
            thinking: None,
            tool_calls: None,
            tool_name: None,
        }
    }

    pub fn assistant(
        content: String,
        thinking: Option<String>,
        tool_calls: Option<serde_json::Value>,
    ) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content),
            thinking,
            tool_calls,
            tool_name: None,
        }
    }

    pub fn tool(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            thinking: None,
            tool_calls: None,
            tool_name: Some(name.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub id: String,
    pub content: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub updated: DateTime<Local>,
    pub path: PathBuf,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub created: DateTime<Local>,
    pub cwd: PathBuf,
    pub model: String,
    pub mode: AgentMode,
    pub blocks: Vec<Block>,
    pub messages: Vec<ChatMessage>,
    pub todos: Vec<Todo>,
    pub prompt_tokens: u32,
    pub eval_tokens: u32,
    #[serde(default)]
    pub updated: DateTime<Local>,
}

impl Session {
    pub fn new(cwd: PathBuf, model: String) -> Self {
        let id = Uuid::new_v4().to_string();
        let now = Local::now();
        Self {
            id,
            title: "New session".into(),
            created: now,
            cwd,
            model,
            mode: AgentMode::Normal,
            blocks: Vec::new(),
            messages: Vec::new(),
            todos: Vec::new(),
            prompt_tokens: 0,
            eval_tokens: 0,
            updated: now,
        }
    }

    pub fn dir(&self) -> PathBuf {
        config::sessions_root(&self.cwd).join(&self.id)
    }

    pub fn plan_path(&self) -> PathBuf {
        self.dir().join("plan.md")
    }

    pub fn transcript_path(&self) -> PathBuf {
        self.dir().join("session.json")
    }

    pub fn has_transcript(&self) -> bool {
        self.blocks.iter().any(|b| {
            matches!(
                b,
                Block::User { .. } | Block::Assistant { .. } | Block::Tool { .. }
            )
        })
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        if !self.has_transcript() {
            return Ok(());
        }
        self.updated = Local::now();
        let dir = self.dir();
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(self.transcript_path(), json)?;
        Ok(())
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn group_label(cwd: &Path) -> String {
        let name = cwd.file_name().and_then(|s| s.to_str()).unwrap_or(".");
        let parent = cwd
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if parent.is_empty() {
            name.to_string()
        } else {
            format!("{parent}-{name}")
        }
    }

    pub fn relative_time(ts: DateTime<Local>) -> String {
        let secs = (Local::now() - ts).num_seconds().max(0);
        if secs < 60 {
            "just now".into()
        } else if secs < 90 {
            "1m ago".into()
        } else if secs < 3600 {
            format!("{}m ago", secs / 60)
        } else if secs < 3600 * 36 {
            format!("{}h ago", secs / 3600)
        } else {
            format!("{}d ago", secs / 86400)
        }
    }

    fn meta_from(s: Session, path: PathBuf) -> SessionMeta {
        SessionMeta {
            id: s.id,
            title: s.title,
            updated: s.updated.max(s.created),
            cwd: s.cwd,
            path,
        }
    }

    pub fn list(cwd: &Path) -> Vec<SessionMeta> {
        Self::gc_blank(cwd);
        Self::metas_in(&config::sessions_root(cwd))
    }

    fn gc_blank(cwd: &Path) {
        let root = config::sessions_root(cwd);
        let Ok(rd) = std::fs::read_dir(&root) else {
            return;
        };
        for ent in rd.flatten() {
            let dir = ent.path();
            if !dir.is_dir() {
                continue;
            }
            let p = dir.join("session.json");
            let blank = match Session::load(&p) {
                Ok(s) => !s.has_transcript(),
                Err(_) => p.exists(),
            };
            if blank {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        if std::fs::read_dir(&root)
            .ok()
            .is_some_and(|mut d| d.next().is_none())
        {
            let _ = std::fs::remove_dir(&root);
        }
    }

    fn metas_in(root: &Path) -> Vec<SessionMeta> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(root) else {
            return out;
        };
        for ent in rd.flatten() {
            let p = ent.path().join("session.json");
            if let Ok(s) = Session::load(&p) {
                if s.has_transcript() {
                    out.push(Self::meta_from(s, p));
                }
            }
        }
        out.sort_by_key(|a| std::cmp::Reverse(a.updated));
        out
    }

    pub fn grouped(list: Vec<&SessionMeta>, expanded: bool) -> Vec<&SessionMeta> {
        let mut buckets: HashMap<String, Vec<&SessionMeta>> = HashMap::new();
        let mut keys: Vec<String> = Vec::new();
        for s in list {
            let g = Self::group_label(&s.cwd);
            let bucket = buckets.entry(g.clone()).or_insert_with(|| {
                keys.push(g.clone());
                Vec::new()
            });
            bucket.push(s);
        }
        keys.sort();
        let mut out = Vec::new();
        for k in keys {
            let mut bucket = buckets.remove(&k).unwrap_or_default();
            bucket.sort_by_key(|s| std::cmp::Reverse(s.updated));
            if expanded {
                out.extend(bucket);
            } else {
                out.extend(bucket.into_iter().take(3));
            }
        }
        out
    }

    pub fn delete(path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            if dir.join("session.json") == path {
                std::fs::remove_dir_all(dir)?;
                return Ok(());
            }
        }
        std::fs::remove_file(path)?;
        Ok(())
    }

    pub fn maybe_title_from(&mut self, text: &str) {
        if self.title == "New session" {
            let t = text.trim().replace('\n', " ");
            self.title = if t.chars().count() > 42 {
                let s: String = t.chars().take(41).collect();
                format!("{s}…")
            } else if t.is_empty() {
                "New session".into()
            } else {
                t
            };
        }
    }

    pub fn last_mut_tool(&mut self, id: &str) -> Option<&mut Block> {
        self.blocks
            .iter_mut()
            .rev()
            .find(|b| matches!(b, Block::Tool { id: tid, .. } if tid == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_cycles_three_ways() {
        assert_eq!(AgentMode::Normal.next(), AgentMode::Plan);
        assert_eq!(AgentMode::Plan.next(), AgentMode::Always);
        assert_eq!(AgentMode::Always.next(), AgentMode::Normal);
    }

    #[test]
    fn blank_session_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new(dir.path().to_path_buf(), "m".into());
        s.blocks.push(Block::Notice {
            text: "mode → Plan".into(),
        });
        s.save().unwrap();
        assert!(
            !s.transcript_path().exists(),
            "empty New session must not hit disk"
        );
        assert!(s
            .dir()
            .starts_with(dir.path().join(".zoder").join("sessions")));
    }

    #[test]
    fn save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new(dir.path().to_path_buf(), "m".into());
        s.blocks.push(Block::User {
            text: "hi".into(),
            time: String::new(),
        });
        s.save().unwrap();
        assert!(s
            .transcript_path()
            .starts_with(dir.path().join(".zoder").join("sessions")));
        let loaded = Session::load(&s.transcript_path()).unwrap();
        assert_eq!(loaded.blocks.len(), 1);
    }

    #[test]
    fn groups_are_alpha_then_recency() {
        let now = Local::now();
        let z = SessionMeta {
            id: "z".into(),
            title: "z".into(),
            updated: now,
            path: PathBuf::from("/tmp/z"),
            cwd: PathBuf::from("/Users/a/Develop/zoder"),
        };
        let a = SessionMeta {
            id: "a".into(),
            title: "a".into(),
            updated: now - chrono::Duration::hours(72),
            path: PathBuf::from("/tmp/a"),
            cwd: PathBuf::from("/Users/a/Develop/assistant"),
        };
        let t = SessionMeta {
            id: "t".into(),
            title: "t".into(),
            updated: now - chrono::Duration::hours(24 * 23),
            path: PathBuf::from("/tmp/t"),
            cwd: PathBuf::from("/Users/a/Develop/tycoon"),
        };
        let list = Session::grouped(vec![&z, &a, &t], true);
        let labels: Vec<String> = list.iter().map(|s| Session::group_label(&s.cwd)).collect();
        assert_eq!(
            labels,
            vec![
                "Develop-assistant".to_string(),
                "Develop-tycoon".to_string(),
                "Develop-zoder".to_string()
            ]
        );
    }

    #[test]
    fn group_label_is_parent_name() {
        assert_eq!(
            Session::group_label(Path::new("/Users/a/Develop/zoder")),
            "Develop-zoder"
        );
        assert_eq!(
            Session::group_label(Path::new("/Users/jirawong/.sixth")),
            "jirawong-.sixth"
        );
    }

    #[test]
    fn delete_removes_session_dir() {
        let dir = tempfile::tempdir().unwrap();
        let sess = dir.path().join("id");
        std::fs::create_dir_all(&sess).unwrap();
        let p = sess.join("session.json");
        std::fs::write(&p, "{}").unwrap();
        Session::delete(&p).unwrap();
        assert!(!sess.exists());
    }
}
