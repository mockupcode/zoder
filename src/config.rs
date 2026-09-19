use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// Public fallback when no user config exists. Local loopback only — no LAN hosts.
pub const FALLBACK_HOST: &str = "http://127.0.0.1:11434";
pub const FALLBACK_KIND: &str = "ollama";
pub const FALLBACK_PROVIDER: &str = "local";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Id of the provider currently in use (`[providers.<id>]`).
    #[serde(default = "default_provider_id")]
    pub provider: String,
    #[serde(default)]
    pub providers: BTreeMap<String, Provider>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    /// Backend kind: `ollama` or `openai`.
    #[serde(default = "default_kind")]
    pub kind: String,
    pub host: String,
    /// Model used for new turns on this provider.
    #[serde(default)]
    pub model: String,
    /// Optional catalog shown in the model picker. Empty = probe the host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// `device` (browser code + API key), `api_key`, or empty (none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth: String,
    /// Env var read for a bearer key (e.g. `XAI_API_KEY`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key_env: String,
    /// Token window for auto-compact. 0 = unknown, auto-compact stays off.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub context_window: u32,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn default_provider_id() -> String {
    FALLBACK_PROVIDER.to_string()
}

fn default_kind() -> String {
    FALLBACK_KIND.to_string()
}

fn provider(kind: &str, host: &str, model: &str, auth: &str, api_key_env: &str) -> Provider {
    Provider {
        kind: kind.to_string(),
        host: host.to_string(),
        model: model.to_string(),
        models: Vec::new(),
        auth: auth.to_string(),
        api_key_env: api_key_env.to_string(),
        context_window: 0,
    }
}

fn fallback_providers() -> BTreeMap<String, Provider> {
    let mut m = BTreeMap::new();
    m.insert(
        FALLBACK_PROVIDER.to_string(),
        provider(FALLBACK_KIND, FALLBACK_HOST, "", "", ""),
    );
    m
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: default_provider_id(),
            providers: fallback_providers(),
        }
    }
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let mut cfg = Self::default();
        if let Some(path) = config_path() {
            if path.exists() {
                let raw = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                cfg =
                    toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
            }
        }
        if cfg.providers.is_empty() {
            cfg.providers = fallback_providers();
        }
        if !cfg.providers.contains_key(&cfg.provider) {
            if let Some(first) = cfg.providers.keys().next().cloned() {
                cfg.provider = first;
            }
        }
        if let Ok(host) = std::env::var("ZODER_HOST") {
            if !host.trim().is_empty() {
                cfg.set_host(normalize_host(&host));
            }
        }
        if let Ok(model) = std::env::var("ZODER_MODEL") {
            if !model.trim().is_empty() {
                cfg.set_model(model);
            }
        }
        if let Ok(p) = std::env::var("ZODER_PROVIDER") {
            if cfg.providers.contains_key(&p) {
                cfg.provider = p;
            }
        }
        Ok(cfg)
    }

    pub fn active(&self) -> Option<&Provider> {
        self.providers.get(&self.provider)
    }

    pub fn active_mut(&mut self) -> Option<&mut Provider> {
        self.providers.get_mut(&self.provider)
    }

    pub fn host(&self) -> &str {
        self.active()
            .map(|p| p.host.as_str())
            .unwrap_or(FALLBACK_HOST)
    }

    pub fn model(&self) -> &str {
        self.active().map(|p| p.model.as_str()).unwrap_or("")
    }

    pub fn kind(&self) -> &str {
        self.active()
            .map(|p| p.kind.as_str())
            .unwrap_or(FALLBACK_KIND)
    }

    pub fn set_host(&mut self, host: String) {
        let host = normalize_host(&host);
        if let Some(p) = self.active_mut() {
            p.host = host;
        }
    }

    pub fn set_model(&mut self, model: String) {
        if let Some(p) = self.active_mut() {
            if !p.models.contains(&model) && !model.is_empty() {
                p.models.push(model.clone());
            }
            p.model = model;
        }
    }

    pub fn select_provider(&mut self, id: &str) -> bool {
        if self.providers.contains_key(id) {
            self.provider = id.to_string();
            true
        } else {
            false
        }
    }

    pub fn remove_provider(&mut self, id: &str) -> anyhow::Result<()> {
        if !self.providers.contains_key(id) {
            anyhow::bail!("unknown provider `{id}`");
        }
        if self.providers.len() == 1 {
            anyhow::bail!("cannot delete the last provider");
        }
        self.providers.remove(id);
        if self.provider == id {
            self.provider = if self.providers.contains_key(FALLBACK_PROVIDER) {
                FALLBACK_PROVIDER.to_string()
            } else {
                self.providers.keys().next().cloned().unwrap_or_default()
            };
        }
        Ok(())
    }

    pub fn api_key_env(&self) -> &str {
        self.active().map(|p| p.api_key_env.as_str()).unwrap_or("")
    }

    pub fn auth(&self) -> &str {
        self.active().map(|p| p.auth.as_str()).unwrap_or("")
    }

    pub fn context_window(&self) -> u32 {
        self.active().map(|p| p.context_window).unwrap_or(0)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        #[cfg(test)]
        {
            return Ok(());
        }
        #[allow(unreachable_code)]
        let Some(path) = config_path() else {
            anyhow::bail!("no config path");
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let raw = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(&path, raw).with_context(|| format!("writing {}", path.display()))
    }
}

/// Config, install bin, and sessions (`~/.zoder`). Override with `ZODER_HOME`.
pub fn home_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ZODER_HOME") {
        return PathBuf::from(p);
    }
    #[cfg(test)]
    {
        return test_home();
    }
    #[allow(unreachable_code)]
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".zoder")
}

#[cfg(test)]
fn test_home() -> PathBuf {
    use std::sync::OnceLock;
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let p = std::env::temp_dir().join(format!("zoder-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&p);
        p
    })
    .clone()
}

pub fn config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".zoder").join("config.toml"))
}

pub fn sessions_root(cwd: &Path) -> PathBuf {
    home_dir().join("sessions").join(encode_cwd(cwd))
}

pub fn encode_cwd(cwd: &Path) -> String {
    cwd.to_string_lossy().replace('/', "%2F")
}

pub fn normalize_host(host: &str) -> String {
    let h = host.trim().trim_end_matches('/');
    if h.starts_with("http://") || h.starts_with("https://") {
        h.to_string()
    } else {
        format!("http://{h}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_gains_scheme() {
        assert_eq!(normalize_host("127.0.0.1:11434"), "http://127.0.0.1:11434");
        assert_eq!(
            normalize_host("http://127.0.0.1:11434/"),
            "http://127.0.0.1:11434"
        );
    }

    #[test]
    fn cwd_encode_is_flat() {
        let s = encode_cwd(Path::new("/Users/demo/proj"));
        assert!(!s.contains('/'), "{s}");
        assert!(s.contains("%2F"));
    }

    #[test]
    fn parses_named_providers() {
        let raw = r#"
provider = "home"

[providers.home]
kind = "ollama"
host = "http://127.0.0.1:11434"
model = "llama3.2"
models = ["llama3.2", "qwen2.5-coder"]

[providers.cloud]
kind = "ollama"
host = "http://127.0.0.1:11435"
model = "other"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.provider, "home");
        assert_eq!(cfg.host(), "http://127.0.0.1:11434");
        assert_eq!(cfg.model(), "llama3.2");
        assert_eq!(cfg.providers.len(), 2);
        let mut cfg = cfg;
        assert!(cfg.select_provider("cloud"));
        assert_eq!(cfg.host(), "http://127.0.0.1:11435");
        cfg.set_model("next".into());
        assert_eq!(cfg.model(), "next");
        assert!(cfg.active().unwrap().models.contains(&"next".to_string()));
    }

    #[test]
    fn default_is_loopback_not_lan() {
        let cfg = Config::default();
        assert_eq!(cfg.host(), FALLBACK_HOST);
        assert!(!cfg.host().contains("192.168."));
        assert_eq!(cfg.providers.len(), 1);
    }

    #[test]
    fn remove_provider_switches_active() {
        let mut cfg = Config::default();
        cfg.providers.insert(
            "grok".into(),
            provider(
                "openai",
                "https://api.x.ai/v1",
                "grok-4.5",
                "device",
                "XAI_API_KEY",
            ),
        );
        cfg.provider = "grok".into();
        cfg.remove_provider("grok").unwrap();
        assert_eq!(cfg.provider, FALLBACK_PROVIDER);
        assert!(cfg.remove_provider(FALLBACK_PROVIDER).is_err());
    }
}
