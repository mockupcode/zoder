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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account_id: String,
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
    id_token: Option<String>,
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
    if let Some(id) = tokens
        .id_token
        .as_deref()
        .and_then(account_id_from_jwt)
        .or_else(|| account_id_from_jwt(&tokens.access_token))
    {
        c.account_id = id;
    }
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

pub fn account_id(provider: &str) -> String {
    creds(provider).account_id
}

async fn refresh(provider: &str, refresh_token: &str) -> anyhow::Result<Creds> {
    let http = http_client()?;
    let (url, client_id) = if provider == "codex" {
        (
            "https://auth.openai.com/oauth/token".to_string(),
            CODEX_CLIENT_ID,
        )
    } else {
        (format!("{ISSUER}/oauth2/token"), CLIENT_ID)
    };
    let res = http
        .post(&url)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
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

const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_ISSUER: &str = "https://auth.openai.com";

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

#[derive(Debug, Deserialize)]
struct CodexUserCode {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(default)]
    interval: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct CodexGrant {
    authorization_code: String,
    code_verifier: String,
}

pub async fn request_codex_device() -> anyhow::Result<DevicePending> {
    let http = http_client()?;
    let res = http
        .post(format!("{CODEX_ISSUER}/api/accounts/deviceauth/usercode"))
        .header("Accept", "application/json")
        .json(&serde_json::json!({ "client_id": CODEX_CLIENT_ID }))
        .send()
        .await?;
    if res.status().as_u16() == 404 {
        anyhow::bail!("device sign-in is not enabled on this account");
    }
    if !res.status().is_success() {
        anyhow::bail!("device auth failed: {}", res.status());
    }
    let body: CodexUserCode = res.json().await?;
    if !body
        .user_code
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        anyhow::bail!("invalid user code from server");
    }
    let interval = body
        .interval
        .as_u64()
        .or_else(|| body.interval.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(5)
        .max(1);
    Ok(DevicePending {
        verification_uri: format!("{CODEX_ISSUER}/codex/device"),
        user_code: body.user_code,
        device_code: body.device_auth_id,
        interval,
        expires_in: 15 * 60,
    })
}

pub async fn complete_codex_device(
    provider: &str,
    pending: DevicePending,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let http = http_client()?;
    let url = format!("{CODEX_ISSUER}/api/accounts/deviceauth/token");
    let interval = Duration::from_secs(pending.interval);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(pending.expires_in);
    let grant = loop {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        if tokio::time::Instant::now() > deadline {
            anyhow::bail!("device code expired — zoder provider add");
        }
        let res = http
            .post(&url)
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "device_auth_id": pending.device_code,
                "user_code": pending.user_code,
            }))
            .send()
            .await?;
        let status = res.status().as_u16();
        if status == 403 || status == 404 {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = wait_cancel(cancel) => anyhow::bail!("cancelled"),
            }
            continue;
        }
        if !res.status().is_success() {
            anyhow::bail!("device auth failed: {status}");
        }
        let g: CodexGrant = res.json().await?;
        break g;
    };
    let res = http
        .post(format!("{CODEX_ISSUER}/oauth/token"))
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CODEX_CLIENT_ID),
            ("code", grant.authorization_code.as_str()),
            ("code_verifier", grant.code_verifier.as_str()),
            (
                "redirect_uri",
                "https://auth.openai.com/deviceauth/callback",
            ),
        ])
        .send()
        .await?;
    if !res.status().is_success() {
        anyhow::bail!("token exchange failed: {}", res.status());
    }
    let tokens: TokenOk = res.json().await?;
    save_tokens(provider, tokens)?;
    Ok(())
}

pub(crate) fn account_id_from_jwt(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = b64url_decode(payload)?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("chatgpt_account_id")
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .or_else(|| {
            v.get("https://api.openai.com/auth")
                .and_then(|a| a.get("chatgpt_account_id"))
                .and_then(|s| s.as_str())
                .map(str::to_string)
        })
        .filter(|s| !s.is_empty())
}

fn b64url_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        })
    }
    let bytes: Vec<u8> = input.bytes().filter(|b| *b != b'=').collect();
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let a = val(bytes[i])?;
        let b = bytes.get(i + 1).copied().and_then(val).unwrap_or(0);
        out.push((a << 2) | (b >> 4));
        if i + 2 < bytes.len() {
            let c = val(bytes[i + 2])?;
            out.push((b << 4) | (c >> 2));
            if i + 3 < bytes.len() {
                let d = val(bytes[i + 3])?;
                out.push((c << 6) | d);
            }
        }
        i += 4;
    }
    Some(out)
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

    #[test]
    fn jwt_account_id() {
        let payload = b64url_encode(br#"{"chatgpt_account_id":"acc-1"}"#);
        let jwt = format!("e30.{payload}.sig");
        assert_eq!(account_id_from_jwt(&jwt).as_deref(), Some("acc-1"));
    }

    fn b64url_encode(bytes: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        let mut i = 0;
        while i < bytes.len() {
            let b0 = bytes[i];
            let b1 = bytes.get(i + 1).copied().unwrap_or(0);
            let b2 = bytes.get(i + 2).copied().unwrap_or(0);
            out.push(T[(b0 >> 2) as usize] as char);
            out.push(T[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
            if i + 1 < bytes.len() {
                out.push(T[(((b1 & 15) << 2) | (b2 >> 6)) as usize] as char);
            }
            if i + 2 < bytes.len() {
                out.push(T[(b2 & 63) as usize] as char);
            }
            i += 3;
        }
        out
    }
}
