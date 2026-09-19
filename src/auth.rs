use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::{self, Config};

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const ISSUER: &str = "https://auth.x.ai";
const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const REFRESH_SKEW_SECS: u64 = 120;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Creds {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub access_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Store {
    #[serde(flatten)]
    providers: BTreeMap<String, Creds>,
}

#[derive(Debug, Clone)]
pub struct DevicePending {
    pub verification_uri: String,
    pub user_code: String,
    device_code: String,
    interval: u64,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct DeviceResp {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenOk {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenErr {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

fn auth_path() -> PathBuf {
    config::home_dir().join("auth.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn load_store() -> Store {
    let path = auth_path();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Store::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn save_store(store: &Store) -> anyhow::Result<()> {
    let path = auth_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let raw = serde_json::to_string_pretty(store)?;
    std::fs::write(path, raw)?;
    Ok(())
}

pub fn creds(provider: &str) -> Creds {
    load_store()
        .providers
        .get(provider)
        .cloned()
        .unwrap_or_default()
}

pub fn set_api_key(provider: &str, key: String) -> anyhow::Result<()> {
    let mut store = load_store();
    store
        .providers
        .entry(provider.to_string())
        .or_default()
        .api_key = key;
    save_store(&store)
}

pub fn clear(provider: &str) -> anyhow::Result<()> {
    let mut store = load_store();
    store.providers.remove(provider);
    save_store(&store)
}

fn save_tokens(provider: &str, tokens: TokenOk) -> anyhow::Result<()> {
    let mut store = load_store();
    let c = store.providers.entry(provider.to_string()).or_default();
    c.access_token = tokens.access_token;
    if let Some(r) = tokens.refresh_token {
        c.refresh_token = r;
    }
    c.expires_at = tokens.expires_in.map(|s| now_secs().saturating_add(s));
    save_store(&store)
}

fn expired(c: &Creds) -> bool {
    match c.expires_at {
        Some(t) => now_secs() + REFRESH_SKEW_SECS >= t,
        None => false,
    }
}

/// Bearer for the active provider: API key (env or stored) wins, then device token.
pub async fn bearer(cfg: &Config) -> Result<String, String> {
    bearer_for(cfg.provider.as_str(), cfg.api_key_env(), cfg.auth()).await
}

pub async fn bearer_for(provider: &str, api_key_env: &str, auth: &str) -> Result<String, String> {
    if !api_key_env.is_empty() {
        if let Ok(v) = std::env::var(api_key_env) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Ok(v);
            }
        }
    }
    let mut c = creds(provider);
    if !c.api_key.trim().is_empty() {
        return Ok(c.api_key.trim().to_string());
    }
    if auth != "device" {
        return Err("no API key — zoder provider add".into());
    }
    if c.access_token.is_empty() {
        return Err("not signed in — zoder provider add".into());
    }
    if expired(&c) && !c.refresh_token.is_empty() {
        c = refresh(provider, &c.refresh_token)
            .await
            .map_err(|e| e.to_string())?;
    }
    if c.access_token.is_empty() {
        return Err("not signed in — zoder provider add".into());
    }
    Ok(c.access_token)
}

async fn refresh(provider: &str, refresh_token: &str) -> anyhow::Result<Creds> {
    let http = http_client()?;
    let url = format!("{ISSUER}/oauth2/token");
    let res = http
        .post(&url)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await?;
    if !res.status().is_success() {
        let _ = clear(provider);
        anyhow::bail!("session expired — zoder provider add");
    }
    let tokens: TokenOk = res.json().await?;
    save_tokens(provider, tokens)?;
    Ok(creds(provider))
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .user_agent("zoder/0.1")
        .build()?)
}

pub async fn request_device() -> anyhow::Result<DevicePending> {
    let http = http_client()?;
    let url = format!("{ISSUER}/oauth2/device/code");
    let res = http
        .post(&url)
        .header("Accept", "application/json")
        .header("x-grok-client-surface", "cli")
        .form(&[
            ("client_id", CLIENT_ID),
            ("scope", SCOPE),
            ("referrer", "zoder"),
        ])
        .send()
        .await?;
    if !res.status().is_success() {
        let t = res.text().await.unwrap_or_default();
        anyhow::bail!("device auth failed: {t}");
    }
    let body: DeviceResp = res.json().await?;
    if !body
        .user_code
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        anyhow::bail!("invalid user code from server");
    }
    let uri = body
        .verification_uri_complete
        .filter(|u| valid_uri(u))
        .unwrap_or(body.verification_uri);
    if !valid_uri(&uri) {
        anyhow::bail!("invalid verification URL from server");
    }
    Ok(DevicePending {
        verification_uri: uri,
        user_code: body.user_code,
        device_code: body.device_code,
        interval: body.interval.unwrap_or(5).max(1),
        expires_in: body.expires_in.max(60),
    })
}

fn valid_uri(uri: &str) -> bool {
    if uri.chars().any(|c| c.is_ascii_control()) {
        return false;
    }
    uri.starts_with("https://")
        || uri.starts_with("http://127.0.0.1")
        || uri.starts_with("http://localhost")
}

pub async fn complete_device(
    provider: &str,
    pending: DevicePending,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let http = http_client()?;
    let url = format!("{ISSUER}/oauth2/token");
    let mut interval = Duration::from_secs(pending.interval);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(pending.expires_in);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = wait_cancel(cancel) => anyhow::bail!("cancelled"),
        }
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        if tokio::time::Instant::now() > deadline {
            anyhow::bail!("device code expired — zoder provider add");
        }
        let res = http
            .post(&url)
            .header("Accept", "application/json")
            .header("x-grok-client-surface", "cli")
            .form(&[
                ("grant_type", DEVICE_GRANT),
                ("device_code", pending.device_code.as_str()),
                ("client_id", CLIENT_ID),
            ])
            .send()
            .await?;
        if res.status().is_success() {
            let tokens: TokenOk = res.json().await?;
            save_tokens(provider, tokens)?;
            return Ok(());
        }
        let err = res.json::<TokenErr>().await.unwrap_or(TokenErr {
            error: "unknown".into(),
            error_description: None,
        });
        match err.error.as_str() {
            "authorization_pending" => continue,
            "slow_down" => {
                interval += Duration::from_secs(5);
                continue;
            }
            "access_denied" => anyhow::bail!("authorization denied"),
            "expired_token" => anyhow::bail!("device code expired — zoder provider add"),
            other => {
                let d = err.error_description.unwrap_or(other.to_string());
                anyhow::bail!("{d}");
            }
        }
    }
}

async fn wait_cancel(flag: &AtomicBool) {
    loop {
        if flag.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

pub fn open_url(url: &str) {
    let url = url.to_string();
    let _ = std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&url).spawn();
        #[cfg(target_os = "linux")]
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
        #[cfg(target_os = "windows")]
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", &url])
            .spawn();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_beats_empty_oauth() {
        let path = auth_path();
        let _ = std::fs::remove_file(&path);
        set_api_key("openrouter", "or-secret".into()).unwrap();
        let c = creds("openrouter");
        assert_eq!(c.api_key, "or-secret");
        clear("openrouter").unwrap();
    }

    #[test]
    fn valid_https_uri() {
        assert!(valid_uri("https://auth.x.ai/device"));
        assert!(!valid_uri("javascript:alert(1)"));
        assert!(!valid_uri("https://x.ai/\n"));
    }
}
