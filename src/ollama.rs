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
        if cfg.host().contains("chatgpt.com") {
            extra_headers.push(("originator".into(), "zoder".into()));
            extra_headers.push(("OpenAI-Beta".into(), "responses=experimental".into()));
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

    fn codex_backend(&self) -> bool {
        self.host.contains("chatgpt.com")
    }

    async fn auth_header(&self) -> Result<Option<String>, String> {
        if !self.openai() {
            return Ok(None);
        }
        Ok(Some(
            crate::auth::bearer_for(&self.provider, &self.api_key_env, &self.auth).await?,
        ))
    }

    fn with_auth(&self, mut req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
        req = req.bearer_auth(token);
        for (k, v) in &self.extra_headers {
            req = req.header(k, v);
        }
        let acc = crate::auth::account_id(&self.provider);
        if !acc.is_empty() {
            req = req.header("chatgpt-account-id", acc);
        }
        req
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

    /// Context window for the current model: Ollama `/api/show`, else the host catalog.
    pub async fn live_context_window(&self) -> u32 {
        if self.model.is_empty() {
            return 0;
        }
        if !self.openai() {
            return self.ollama_context_window().await;
        }
        match self.fetch_models_json().await {
            Ok(body) => parse_model_context(&body, &self.model),
            Err(_) => 0,
        }
    }

    async fn ollama_context_window(&self) -> u32 {
        let url = format!("{}/api/show", self.host.trim_end_matches('/'));
        let Ok(res) = self
            .http
            .post(&url)
            .json(&json!({ "model": self.model }))
            .send()
            .await
        else {
            return 0;
        };
        if !res.status().is_success() {
            return 0;
        }
        let Ok(body) = res.json::<Value>().await else {
            return 0;
        };
        parse_ollama_context(&body)
    }

    async fn fetch_models_json(&self) -> Result<Value, String> {
        let token = self
            .auth_header()
            .await?
            .ok_or_else(|| "no token".to_string())?;
        let mut url = format!("{}/models", self.host.trim_end_matches('/'));
        if self.codex_backend() {
            url.push_str("?client_version=999.0.0");
        }
        let req = self.with_auth(self.http.get(&url), &token);
        let res = req.send().await.map_err(|e| e.to_string())?;
        if res.status().as_u16() == 401 {
            return Err("not signed in — zoder provider add".into());
        }
        if !res.status().is_success() {
            return Err(format!("status {}", res.status()));
        }
        res.json().await.map_err(|e| e.to_string())
    }

    async fn probe_openai(&self) -> Result<Vec<String>, String> {
        let body = self.fetch_models_json().await?;
        Ok(parse_model_ids(&body))
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
            if self.codex_backend() {
                return self
                    .stream_responses(messages, tools, cancel, on_chunk)
                    .await;
            }
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
        let req = self.with_auth(self.http.post(&url).json(&body), &token);
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

    async fn stream_responses(
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
        let (instructions, input) = responses_input(messages);
        let mut body = json!({
            "model": self.model,
            "input": input,
            "stream": true,
            "store": false,
        });
        if !instructions.is_empty() {
            body["instructions"] = json!(instructions);
        }
        let rtools = responses_tools(tools);
        if !rtools.is_empty() {
            body["tools"] = json!(rtools);
        }
        let url = format!("{}/responses", self.host.trim_end_matches('/'));
        let req = self.with_auth(self.http.post(&url).json(&body), &token);
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
            for (ev, data) in take_sse(&mut buf) {
                if data == "[DONE]" {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&data) else {
                    continue;
                };
                if let Some(err) = v.get("error") {
                    let msg = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("chat failed");
                    anyhow::bail!("{msg}");
                }
                let kind = if ev.is_empty() {
                    v.get("type").and_then(|t| t.as_str()).unwrap_or("")
                } else {
                    ev.as_str()
                };
                match kind {
                    "response.output_text.delta" => {
                        let piece = text_of(v.get("delta"));
                        if !piece.is_empty() {
                            content.push_str(&piece);
                            on_chunk(ChatChunk {
                                message: ChunkMessage {
                                    role: "assistant".into(),
                                    content: piece,
                                    thinking: String::new(),
                                    tool_calls: Vec::new(),
                                },
                                done: false,
                                prompt_eval_count: None,
                                eval_count: None,
                                error: None,
                            });
                        }
                    }
                    "response.reasoning_summary_text.delta"
                    | "response.reasoning_text.delta"
                    | "response.reasoning.delta" => {
                        let think = text_of(v.get("delta"));
                        if !think.is_empty() {
                            thinking.push_str(&think);
                            on_chunk(ChatChunk {
                                message: ChunkMessage {
                                    role: "assistant".into(),
                                    content: String::new(),
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
                    "response.output_item.added" | "response.output_item.done" => {
                        apply_response_item(&mut acc, v.get("item"));
                    }
                    "response.function_call_arguments.delta" => {
                        let delta = text_of(v.get("delta"));
                        if delta.is_empty() {
                            continue;
                        }
                        if let Some(last) = acc.last_mut() {
                            last.arguments.push_str(&delta);
                        } else {
                            acc.push(OaiCall {
                                arguments: delta,
                                ..OaiCall::default()
                            });
                        }
                    }
                    "response.completed" | "response.incomplete" => {
                        if let Some(u) = v.pointer("/response/usage") {
                            prompt_tokens = num(u, "input_tokens");
                            eval_tokens = num(u, "output_tokens");
                        }
                    }
                    "response.failed" => {
                        let msg = v
                            .pointer("/response/error/message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("chat failed");
                        anyhow::bail!("{msg}");
                    }
                    _ => {}
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

pub(crate) fn responses_input(messages: &[ChatMessage]) -> (String, Vec<Value>) {
    let mut instructions = String::new();
    let mut input = Vec::new();
    let mut pending_ids: Vec<String> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        match m.role.as_str() {
            "system" => {
                if !instructions.is_empty() {
                    instructions.push('\n');
                }
                instructions.push_str(m.content.as_deref().unwrap_or(""));
            }
            "tool" => {
                let id = if pending_ids.is_empty() {
                    format!("call-{i}")
                } else {
                    pending_ids.remove(0)
                };
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": id,
                    "output": m.content.clone().unwrap_or_default(),
                }));
            }
            "assistant" => {
                pending_ids.clear();
                if let Some(tc) = &m.tool_calls {
                    for c in openai_tool_calls(tc) {
                        let id = c
                            .get("id")
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = c
                            .pointer("/function/name")
                            .and_then(|s| s.as_str())
                            .unwrap_or("");
                        let args = c
                            .pointer("/function/arguments")
                            .and_then(|s| s.as_str())
                            .unwrap_or("{}");
                        pending_ids.push(id.clone());
                        input.push(json!({
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": args,
                        }));
                    }
                }
                if let Some(text) = &m.content {
                    if !text.is_empty() {
                        input.push(json!({
                            "role": "assistant",
                            "content": text,
                        }));
                    }
                }
            }
            other => {
                pending_ids.clear();
                input.push(json!({
                    "role": other,
                    "content": m.content.clone().unwrap_or_default(),
                }));
            }
        }
    }
    (instructions, input)
}

fn responses_tools(tools: &Value) -> Vec<Value> {
    let Some(arr) = tools.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|t| {
            let fn_ = t.get("function")?;
            Some(json!({
                "type": "function",
                "name": fn_.get("name")?,
                "description": fn_.get("description").cloned().unwrap_or(json!("")),
                "parameters": fn_.get("parameters").cloned().unwrap_or(json!({})),
            }))
        })
        .collect()
}

fn apply_response_item(acc: &mut Vec<OaiCall>, item: Option<&Value>) {
    let Some(item) = item else {
        return;
    };
    if item.get("type").and_then(|t| t.as_str()) != Some("function_call") {
        return;
    }
    let name = item
        .get("name")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let args = item
        .get("arguments")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    if let Some(existing) = acc.iter_mut().find(|c| !id.is_empty() && c.id == id) {
        if !name.is_empty() {
            existing.name = name;
        }
        if !args.is_empty() {
            existing.arguments = args;
        }
        return;
    }
    if name.is_empty() && args.is_empty() {
        return;
    }
    acc.push(OaiCall {
        id,
        name,
        arguments: args,
    });
}

pub(crate) fn parse_ollama_context(body: &Value) -> u32 {
    if let Some(p) = body.get("parameters").and_then(|s| s.as_str()) {
        for line in p.lines() {
            let mut parts = line.split_whitespace();
            if parts.next() == Some("num_ctx") {
                if let Some(n) = parts.next().and_then(|s| s.parse().ok()) {
                    if n > 0 {
                        return n;
                    }
                }
            }
        }
    }
    let mut best = 0u32;
    if let Some(info) = body.get("model_info").and_then(|o| o.as_object()) {
        for (k, val) in info {
            if !k.ends_with("context_length") {
                continue;
            }
            let n = val
                .as_u64()
                .or_else(|| val.as_i64().map(|i| i as u64))
                .unwrap_or(0);
            if n > 0 && n <= u32::MAX as u64 {
                best = best.max(n as u32);
            }
        }
    }
    best
}

fn json_u32(v: Option<&Value>) -> u32 {
    let Some(v) = v else {
        return 0;
    };
    let n = v
        .as_u64()
        .or_else(|| v.as_i64().map(|i| i.max(0) as u64))
        .or_else(|| v.as_f64().map(|f| f.max(0.0) as u64))
        .unwrap_or(0);
    if n == 0 || n > u32::MAX as u64 {
        0
    } else {
        n as u32
    }
}

fn context_of_entry(m: &Value) -> u32 {
    let budget = json_u32(m.get("context_window"));
    if budget > 0 {
        return budget;
    }
    let advertised = json_u32(m.get("context_length"));
    let provider = json_u32(m.get("top_provider").and_then(|p| p.get("context_length")));
    match (advertised, provider) {
        (a, p) if a > 0 && p > 0 => a.min(p),
        (a, _) if a > 0 => a,
        (_, p) if p > 0 => p,
        _ => json_u32(m.get("max_context_window")),
    }
}

fn model_entry_id(m: &Value) -> Option<&str> {
    m.get("id")
        .or_else(|| m.get("slug"))
        .or_else(|| m.get("name"))
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
}

pub(crate) fn parse_model_context(body: &Value, model: &str) -> u32 {
    let mut fallback = 0;
    for arr_key in ["data", "models"] {
        let Some(arr) = body.get(arr_key).and_then(|d| d.as_array()) else {
            continue;
        };
        for m in arr {
            let Some(id) = model_entry_id(m) else {
                continue;
            };
            let n = context_of_entry(m);
            if n == 0 {
                continue;
            }
            if id == model {
                return n;
            }
            let id_tail = id.rsplit('/').next().unwrap_or(id);
            let model_tail = model.rsplit('/').next().unwrap_or(model);
            if fallback == 0 && (id_tail == model || model_tail == id) {
                fallback = n;
            }
        }
    }
    fallback
}

pub(crate) fn parse_model_ids(body: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(arr) = body.get("data").and_then(|d| d.as_array()) {
        for m in arr {
            if let Some(id) = m.get("id").and_then(|i| i.as_str()) {
                if !id.is_empty() {
                    ids.push(id.to_string());
                }
            }
        }
    }
    if ids.is_empty() {
        if let Some(arr) = body.get("models").and_then(|d| d.as_array()) {
            for m in arr {
                if let Some(vis) = m.get("visibility").and_then(|v| v.as_str()) {
                    if vis != "list" {
                        continue;
                    }
                }
                let id = m
                    .get("slug")
                    .or_else(|| m.get("id"))
                    .or_else(|| m.get("name"))
                    .and_then(|s| s.as_str());
                if let Some(id) = id {
                    if !id.is_empty() {
                        ids.push(id.to_string());
                    }
                }
            }
        }
    }
    ids
}

fn take_sse(buf: &mut String) -> Vec<(String, String)> {
    let mut out = Vec::new();
    while let Some(idx) = buf.find("\n\n") {
        let block = buf[..idx].to_string();
        buf.drain(..idx + 2);
        let mut ev = String::new();
        let mut data = String::new();
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("event:") {
                ev = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest.trim());
            }
        }
        if !data.is_empty() {
            out.push((ev, data));
        }
    }
    out
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

    #[test]
    fn responses_input_maps_tools() {
        let msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("hi"),
            ChatMessage::assistant(
                String::new(),
                None,
                Some(json!([{"function":{"name":"view","arguments":{"file_path":"a.rs"}}}])),
            ),
            ChatMessage::tool("view", "ok"),
        ];
        let (ins, input) = responses_input(&msgs);
        assert_eq!(ins, "sys");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["name"], "view");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], input[1]["call_id"]);
    }

    #[test]
    fn parse_ollama_context_prefers_num_ctx() {
        let body = json!({
            "parameters": "num_keep 24\nnum_ctx 4096\nstop <|eot_id|>",
            "model_info": { "llama.context_length": 131072 }
        });
        assert_eq!(parse_ollama_context(&body), 4096);
        let arch = json!({ "model_info": { "qwen2.context_length": 32768 } });
        assert_eq!(parse_ollama_context(&arch), 32768);
    }

    #[test]
    fn parse_model_context_from_each_host() {
        let openrouter = json!({
            "data": [{
                "id": "anthropic/claude-sonnet-4",
                "context_length": 200000,
                "top_provider": { "context_length": 128000 }
            }]
        });
        assert_eq!(
            parse_model_context(&openrouter, "anthropic/claude-sonnet-4"),
            128000
        );
        let grok = json!({
            "data": [{ "id": "grok-4.6", "context_length": 500000 }]
        });
        assert_eq!(parse_model_context(&grok, "grok-4.6"), 500000);
        let catalog = json!({
            "models": [{
                "slug": "gpt-5.3-codex",
                "context_window": 272000,
                "max_context_window": 400000
            }]
        });
        assert_eq!(parse_model_context(&catalog, "gpt-5.3-codex"), 272000);
        let openai = json!({
            "data": [{ "id": "gpt-5", "object": "model" }]
        });
        assert_eq!(parse_model_context(&openai, "gpt-5"), 0);
    }

    #[test]
    fn parse_openai_and_codex_model_lists() {
        let openai = json!({"data":[{"id":"gpt-5.3-codex"},{"id":"gpt-4o"}]});
        assert_eq!(
            parse_model_ids(&openai),
            vec!["gpt-5.3-codex".to_string(), "gpt-4o".to_string()]
        );
        let catalog = json!({
            "models": [
                {"slug":"gpt-5.3-codex","visibility":"list"},
                {"slug":"hidden","visibility":"internal"},
                {"slug":"gpt-5.1-codex","visibility":"list"}
            ]
        });
        assert_eq!(
            parse_model_ids(&catalog),
            vec!["gpt-5.3-codex".to_string(), "gpt-5.1-codex".to_string()]
        );
    }
}
