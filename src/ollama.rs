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
    pub kind: String,
    pub provider: String,
    api_key_env: String,
    auth: String,
    extra_headers: Vec<(String, String)>,
}

impl Client {
    pub fn from_config(cfg: &Config) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(8))
            .timeout(std::time::Duration::from_secs(600))
            .user_agent("zoder/0.1")
            .build()
            .expect("http client");
        let mut extra_headers = Vec::new();
        if cfg.provider == "openrouter" || cfg.host().contains("openrouter.ai") {
            extra_headers.push((
                "HTTP-Referer".into(),
                "https://github.com/mockupcode/zoder".into(),
            ));
            extra_headers.push(("X-Title".into(), "zoder".into()));
        }
        Self {
            http,
            host: cfg.host().to_string(),
            model: cfg.model().to_string(),
            kind: cfg.kind().to_string(),
            provider: cfg.provider.clone(),
            api_key_env: cfg.api_key_env().to_string(),
            auth: cfg.auth().to_string(),
            extra_headers,
        }
    }

    fn openai(&self) -> bool {
        self.kind == "openai"
    }

    async fn auth_header(&self) -> Result<Option<String>, String> {
        if !self.openai() {
            return Ok(None);
        }
        Ok(Some(
            crate::auth::bearer_for(&self.provider, &self.api_key_env, &self.auth).await?,
        ))
    }

    pub async fn probe(&self) -> Result<Vec<String>, String> {
        if self.openai() {
            return self.probe_openai().await;
        }
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

    async fn probe_openai(&self) -> Result<Vec<String>, String> {
        let token = self
            .auth_header()
            .await?
            .ok_or_else(|| "no token".to_string())?;
        let url = format!("{}/models", self.host.trim_end_matches('/'));
        let mut req = self.http.get(&url).bearer_auth(&token);
        for (k, v) in &self.extra_headers {
            req = req.header(k, v);
        }
        let res = req.send().await.map_err(|e| e.to_string())?;
        if res.status().as_u16() == 401 {
            return Err("not signed in — zoder provider add".into());
        }
        if !res.status().is_success() {
            return Err(format!("status {}", res.status()));
        }
        let body: Value = res.json().await.map_err(|e| e.to_string())?;
        let ids = body
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Ok(ids)
    }

    pub async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: &Value,
        think: bool,
        cancel: &AtomicBool,
        on_chunk: impl FnMut(ChatChunk),
    ) -> anyhow::Result<(String, String, Vec<ToolCall>, u32, u32)> {
        if self.openai() {
            return self.stream_openai(messages, tools, cancel, on_chunk).await;
        }
        self.stream_ollama(messages, tools, think, cancel, on_chunk)
            .await
    }

    async fn stream_ollama(
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

    async fn stream_openai(
        &self,
        messages: &[ChatMessage],
        tools: &Value,
        cancel: &AtomicBool,
        mut on_chunk: impl FnMut(ChatChunk),
    ) -> anyhow::Result<(String, String, Vec<ToolCall>, u32, u32)> {
        let token = self
            .auth_header()
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .ok_or_else(|| anyhow::anyhow!("no token"))?;
        let url = format!("{}/chat/completions", self.host.trim_end_matches('/'));
        let mut body = json!({
            "model": self.model,
            "messages": openai_messages(messages),
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if !tools.is_null() && tools.as_array().is_none_or(|a| !a.is_empty()) {
            body["tools"] = tools.clone();
        }
        let mut req = self.http.post(&url).bearer_auth(&token).json(&body);
        for (k, v) in &self.extra_headers {
            req = req.header(k, v);
        }
        let res = req.send().await?;
        if !res.status().is_success() {
            let t = res.text().await.unwrap_or_default();
            anyhow::bail!("chat failed: {t}");
        }
        let mut stream = res.bytes_stream();
        let mut buf = String::new();
        let mut content = String::new();
        let mut thinking = String::new();
        let mut acc: Vec<OaiCall> = Vec::new();
        let mut prompt_tokens = 0u32;
        let mut eval_tokens = 0u32;
        loop {
            if cancel.load(Ordering::Relaxed) {
                anyhow::bail!("cancelled");
            }
            let item = tokio::select! {
                item = stream.next() => item,
                _ = wait_cancel(cancel) => anyhow::bail!("cancelled"),
            };
            let Some(item) = item else {
                break;
            };
            buf.push_str(&String::from_utf8_lossy(&item?));
            while let Some(idx) = buf.find('\n') {
                let line = buf[..idx].trim().to_string();
                buf.drain(..=idx);
                if line.is_empty() || line.starts_with(':') {
                    continue;
                }
                let payload = line.strip_prefix("data:").unwrap_or(&line).trim();
                if payload.is_empty() {
                    continue;
                }
                if payload == "[DONE]" {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };
                if let Some(err) = v.get("error") {
                    let msg = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("chat failed");
                    anyhow::bail!("{msg}");
                }
                if let Some(u) = v.get("usage") {
                    prompt_tokens = num(u, "prompt_tokens");
                    eval_tokens = num(u, "completion_tokens");
                }
                let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
                    continue;
                };
                let delta = choice.get("delta").unwrap_or(choice);
                let piece = text_of(delta.get("content"));
                let think = text_of(
                    delta
                        .get("reasoning_content")
                        .or_else(|| delta.get("reasoning"))
                        .or_else(|| delta.get("thinking")),
                );
                apply_tool_deltas(&mut acc, delta.get("tool_calls"));
                if !piece.is_empty() {
                    content.push_str(&piece);
                }
                if !think.is_empty() {
                    thinking.push_str(&think);
                }
                if !piece.is_empty() || !think.is_empty() {
                    on_chunk(ChatChunk {
                        message: ChunkMessage {
                            role: "assistant".into(),
                            content: piece,
                            thinking: think,
                            tool_calls: Vec::new(),
                        },
                        done: false,
                        prompt_eval_count: None,
                        eval_count: None,
                        error: None,
                    });
                }
            }
        }
        let calls = finish_calls(acc);
        Ok((content, thinking, calls, prompt_tokens, eval_tokens))
    }
}

#[derive(Default)]
struct OaiCall {
    id: String,
    name: String,
    arguments: String,
}

fn num(v: &Value, key: &str) -> u32 {
    v.get(key).and_then(|n| n.as_u64()).unwrap_or(0) as u32
}

fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                p.as_str()
                    .map(str::to_string)
                    .or_else(|| p.get("text").and_then(|t| t.as_str()).map(str::to_string))
            })
            .collect(),
        _ => String::new(),
    }
}

fn apply_tool_deltas(acc: &mut Vec<OaiCall>, tool_calls: Option<&Value>) {
    let Some(Value::Array(arr)) = tool_calls else {
        return;
    };
    for item in arr {
        let idx = item.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
        while acc.len() <= idx {
            acc.push(OaiCall::default());
        }
        let slot = &mut acc[idx];
        if let Some(id) = item.get("id").and_then(|s| s.as_str()) {
            if !id.is_empty() {
                slot.id = id.to_string();
            }
        }
        if let Some(fn_) = item.get("function") {
            if let Some(n) = fn_.get("name").and_then(|s| s.as_str()) {
                if !n.is_empty() {
                    slot.name.push_str(n);
                }
            }
            if let Some(a) = fn_.get("arguments").and_then(|s| s.as_str()) {
                slot.arguments.push_str(a);
            }
        }
    }
}

fn finish_calls(acc: Vec<OaiCall>) -> Vec<ToolCall> {
    acc.into_iter()
        .filter(|c| !c.name.is_empty())
        .map(|c| {
            let arguments = serde_json::from_str(&c.arguments).unwrap_or_else(|_| json!({}));
            ToolCall {
                function: ToolFn {
                    name: c.name,
                    arguments,
                },
            }
        })
        .collect()
}

pub(crate) fn openai_messages(messages: &[ChatMessage]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut pending_ids: Vec<String> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        match m.role.as_str() {
            "tool" => {
                let id = if pending_ids.is_empty() {
                    format!("call-{i}")
                } else {
                    pending_ids.remove(0)
                };
                let mut obj = json!({
                    "role": "tool",
                    "content": m.content.clone().unwrap_or_default(),
                    "tool_call_id": id,
                });
                if let Some(n) = &m.tool_name {
                    obj["name"] = json!(n);
                }
                out.push(obj);
            }
            "assistant" => {
                pending_ids.clear();
                let mut obj = json!({
                    "role": "assistant",
                    "content": m.content.clone().unwrap_or_default(),
                });
                if let Some(tc) = &m.tool_calls {
                    let converted = openai_tool_calls(tc);
                    pending_ids = converted
                        .iter()
                        .filter_map(|c| c.get("id").and_then(|i| i.as_str()).map(str::to_string))
                        .collect();
                    if !converted.is_empty() {
                        obj["tool_calls"] = Value::Array(converted);
                    }
                }
                out.push(obj);
            }
            other => {
                pending_ids.clear();
                out.push(json!({
                    "role": other,
                    "content": m.content.clone().unwrap_or_default(),
                }));
            }
        }
    }
    out
}

fn openai_tool_calls(raw: &Value) -> Vec<Value> {
    let Some(arr) = raw.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .enumerate()
        .map(|(i, c)| {
            if c.get("id").is_some() && c.get("function").is_some() {
                let mut v = c.clone();
                if v.get("type").is_none() {
                    v["type"] = json!("function");
                }
                if let Some(args) = v
                    .pointer_mut("/function/arguments")
                    .filter(|a| !a.is_string())
                {
                    *args = json!(args.to_string());
                }
                return v;
            }
            let fn_ = c.get("function").unwrap_or(c);
            let name = fn_
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args = fn_.get("arguments").cloned().unwrap_or(json!({}));
            let args_s = if args.is_string() {
                args.as_str().unwrap_or("{}").to_string()
            } else {
                args.to_string()
            };
            json!({
                "id": format!("call-{i}-{name}"),
                "type": "function",
                "function": { "name": name, "arguments": args_s },
            })
        })
        .collect()
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

    #[test]
    fn openai_messages_map_tool_calls() {
        let msgs = vec![
            ChatMessage::user("hi"),
            ChatMessage::assistant(
                String::new(),
                None,
                Some(json!([{"function":{"name":"view","arguments":{"file_path":"a.rs"}}}])),
            ),
            ChatMessage::tool("view", "ok"),
        ];
        let out = openai_messages(&msgs);
        assert_eq!(out[1]["tool_calls"][0]["function"]["name"], "view");
        assert_eq!(out[1]["tool_calls"][0]["type"], "function");
        assert!(out[1]["tool_calls"][0]["function"]["arguments"].is_string());
        assert_eq!(out[2]["tool_call_id"], out[1]["tool_calls"][0]["id"]);
    }
}
