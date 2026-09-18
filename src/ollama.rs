use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::Config;
use crate::session::ChatMessage;

#[derive(Debug, Clone, Deserialize)]
pub struct ChatChunk {
    #[serde(default)]
    pub message: ChunkMessage,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub prompt_eval_count: Option<u32>,
    #[serde(default)]
    pub eval_count: Option<u32>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChunkMessage {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub thinking: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(default)]
    pub function: ToolFn,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolFn {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Deserialize)]
struct Tags {
    #[serde(default)]
    models: Vec<Tag>,
}

#[derive(Debug, Clone, Deserialize)]
struct Tag {
    name: String,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    pub host: String,
    pub model: String,
}

impl Client {
    pub fn from_config(cfg: &Config) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(8))
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .expect("http client");
        Self {
            http,
            host: cfg.host().to_string(),
            model: cfg.model().to_string(),
        }
    }

    pub async fn probe(&self) -> Result<Vec<String>, String> {
        let url = format!("{}/api/tags", self.host.trim_end_matches('/'));
        let res = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("status {}", res.status()));
        }
        let tags: Tags = res.json().await.map_err(|e| e.to_string())?;
        Ok(tags.models.into_iter().map(|m| m.name).collect())
    }

    pub async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: &Value,
        think: bool,
        cancel: &AtomicBool,
        mut on_chunk: impl FnMut(ChatChunk),
    ) -> anyhow::Result<(String, String, Vec<ToolCall>, u32, u32)> {
        let url = format!("{}/api/chat", self.host.trim_end_matches('/'));
        let body = json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "stream": true,
            "think": think,
        });
        let res = self.http.post(&url).json(&body).send().await?;
        if !res.status().is_success() {
            let t = res.text().await.unwrap_or_default();
            anyhow::bail!("chat failed: {t}");
        }
        let mut stream = res.bytes_stream();
        let mut buf = String::new();
        let mut content = String::new();
        let mut thinking = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut prompt_tokens = 0u32;
        let mut eval_tokens = 0u32;
        loop {
            if cancel.load(Ordering::Relaxed) {
                anyhow::bail!("cancelled");
            }
            let item = tokio::select! {
                item = stream.next() => item,
                _ = wait_cancel(cancel) => {
                    anyhow::bail!("cancelled");
                }
            };
            let Some(item) = item else {
                break;
            };
            let bytes = item?;
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(idx) = buf.find('\n') {
                let line = buf[..idx].trim().to_string();
                buf.drain(..=idx);
                if line.is_empty() {
                    continue;
                }
                let chunk: ChatChunk = match serde_json::from_str(&line) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if let Some(err) = &chunk.error {
                    anyhow::bail!("{err}");
                }
                if !chunk.message.thinking.is_empty() {
                    thinking.push_str(&chunk.message.thinking);
                }
                if !chunk.message.content.is_empty() {
                    content.push_str(&chunk.message.content);
                }
                if !chunk.message.tool_calls.is_empty() {
                    calls.extend(chunk.message.tool_calls.clone());
                }
                if let Some(n) = chunk.prompt_eval_count {
                    prompt_tokens = n;
                }
                if let Some(n) = chunk.eval_count {
                    eval_tokens = n;
                }
                on_chunk(chunk);
                if cancel.load(Ordering::Relaxed) {
                    anyhow::bail!("cancelled");
                }
            }
        }
        if !buf.trim().is_empty() {
            if let Ok(chunk) = serde_json::from_str::<ChatChunk>(buf.trim()) {
                if !chunk.message.thinking.is_empty() {
                    thinking.push_str(&chunk.message.thinking);
                }
                if !chunk.message.content.is_empty() {
                    content.push_str(&chunk.message.content);
                }
                if !chunk.message.tool_calls.is_empty() {
                    calls.extend(chunk.message.tool_calls);
                }
            }
        }
        Ok((content, thinking, calls, prompt_tokens, eval_tokens))
    }
}

async fn wait_cancel(flag: &AtomicBool) {
    loop {
        if flag.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }
}

/// Parse a single NDJSON chat line. Used by tests and the client.
pub fn parse_chunk(line: &str) -> Option<ChatChunk> {
    serde_json::from_str(line.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_thinking_and_tool_call() {
        let line = r#"{"message":{"role":"assistant","content":"","thinking":"hmm","tool_calls":[{"function":{"name":"view","arguments":{"file_path":"a.rs"}}}]},"done":false}"#;
        let c = parse_chunk(line).unwrap();
        assert_eq!(c.message.thinking, "hmm");
        assert_eq!(c.message.tool_calls[0].function.name, "view");
        assert_eq!(
            c.message.tool_calls[0].function.arguments["file_path"],
            "a.rs"
        );
    }

    #[test]
    fn parses_content_delta() {
        let c = parse_chunk(r#"{"message":{"role":"assistant","content":"Hello"},"done":false}"#)
            .unwrap();
        assert_eq!(c.message.content, "Hello");
    }
}
