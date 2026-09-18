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
    pub always_approve: bool,
    #[serde(default)]
    pub providers: BTreeMap<String, Provider>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    /// Backend kind. Today: `ollama`. Reserved for later adapters.
    #[serde(default = "default_kind")]
    pub kind: String,
    pub host: String,
    /// Model used for new turns on this provider.
    #[serde(default)]
    pub model: String,
    /// Optional catalog shown in the model picker. Empty = probe the host.
    #[serde(default)]
    pub models: Vec<String>,
}

fn default_provider_id() -> String {
    FALLBACK_PROVIDER.to_string()
}

fn default_kind() -> String {
    FALLBACK_KIND.to_string()
}

fn fallback_providers() -> BTreeMap<String, Provider> {
    let mut m = BTreeMap::new();
    m.insert(
        FALLBACK_PROVIDER.to_string(),
        Provider {
            kind: default_kind(),
            host: FALLBACK_HOST.to_string(),
            model: String::new(),
            models: Vec::new(),
        },
    );
    m
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: default_provider_id(),
            always_approve: false,
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
}

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
always_approve = false

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
    }
}
