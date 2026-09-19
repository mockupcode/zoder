use std::io::{self, BufRead, Write};
use std::sync::atomic::AtomicBool;

use crate::config::{self, Config, Provider, FALLBACK_KIND, FALLBACK_PROVIDER};
use crate::{auth, config::normalize_host};

const USAGE: &str = "\
zoder provider list
zoder provider add
zoder provider delete [id]
";

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("list") | Some("ls") => list(),
        Some("add") => add().await,
        Some("delete") | Some("rm") | Some("remove") => delete(args.get(1).cloned()).await,
        Some("-h" | "--help" | "help") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown provider command `{other}`\n{USAGE}"),
    }
}

fn list() -> anyhow::Result<()> {
    let cfg = Config::load().unwrap_or_default();
    for line in list_lines(&cfg) {
        println!("{line}");
    }
    Ok(())
}

pub(crate) fn list_lines(cfg: &Config) -> Vec<String> {
    cfg.providers
        .iter()
        .map(|(id, p)| {
            let mark = if id == &cfg.provider { "*" } else { " " };
            format!(
                "{mark} {id:<16} {kind:<8} {host}",
                kind = p.kind,
                host = p.host
            )
        })
        .collect()
}

async fn add() -> anyhow::Result<()> {
    let mut out = io::stdout();
    let mut input = io::stdin().lock();
    writeln!(
        out,
        "Add a provider\n  1) grok         device sign-in or API key\n  2) openrouter   API key\n  3) codex        device sign-in or API key\n  4) ollama       local host"
    )?;
    let choice = prompt(&mut input, &mut out, "> ")?;
    let (id, mut p) = match choice.as_str() {
        "1" | "grok" => preset_grok(),
        "2" | "openrouter" => preset_openrouter(),
        "3" | "codex" => preset_codex(),
        "4" | "ollama" | "local" => {
            let host = prompt(&mut input, &mut out, "host [http://127.0.0.1:11434]: ")?;
            let host = if host.is_empty() {
                config::FALLBACK_HOST.to_string()
            } else {
                normalize_host(&host)
            };
            let id = prompt(&mut input, &mut out, "id [local]: ")?;
            let id = if id.is_empty() {
                FALLBACK_PROVIDER.to_string()
            } else {
                id
            };
            (
                id,
                Provider {
                    kind: FALLBACK_KIND.to_string(),
                    host,
                    model: String::new(),
                    models: Vec::new(),
                    auth: String::new(),
                    api_key_env: String::new(),
                },
            )
        }
        _ => anyhow::bail!("pick 1, 2, 3, or 4"),
    };

    let mut cfg = Config::load().unwrap_or_default();
    if cfg.providers.contains_key(&id) && p.kind != FALLBACK_KIND {
        writeln!(out, "`{id}` already exists — will update credentials")?;
    }

    match p.auth.as_str() {
        "device" => {
            writeln!(
                out,
                "Sign in\n  1) device  (open a URL, enter a code)\n  2) api key"
            )?;
            let how = prompt(&mut input, &mut out, "> ")?;
            match how.as_str() {
                "1" | "device" => device_login(&id).await?,
                "2" | "key" | "api" | "api key" | "api_key" => {
                    if id == "codex" {
                        p.host = "https://api.openai.com/v1".into();
                    }
                    paste_key(&mut input, &mut out, &id, &p.api_key_env)?
                }
                _ => anyhow::bail!("pick 1 or 2"),
            }
        }
        "api_key" => paste_key(&mut input, &mut out, &id, &p.api_key_env)?,
        _ => {}
    }

    cfg.providers.insert(id.clone(), p);
    cfg.provider = id.clone();
    cfg.save()?;
    writeln!(out, "saved `{id}` as the active provider")?;
    Ok(())
}

async fn delete(id: Option<String>) -> anyhow::Result<()> {
    let mut cfg = Config::load().unwrap_or_default();
    let id = match id {
        Some(id) => id,
        None => {
            let mut out = io::stdout();
            let mut input = io::stdin().lock();
            writeln!(out, "Delete which provider?")?;
            for (i, name) in cfg.providers.keys().enumerate() {
                writeln!(out, "  {}) {name}", i + 1)?;
            }
            let ans = prompt(&mut input, &mut out, "> ")?;
            pick_id(&cfg, &ans).ok_or_else(|| anyhow::anyhow!("unknown provider"))?
        }
    };
    cfg.remove_provider(&id)?;
    let _ = auth::clear(&id);
    cfg.save()?;
    println!("deleted `{id}`");
    Ok(())
}

fn pick_id(cfg: &Config, ans: &str) -> Option<String> {
    if let Ok(n) = ans.parse::<usize>() {
        if n >= 1 {
            return cfg.providers.keys().nth(n - 1).cloned();
        }
    }
    cfg.providers.contains_key(ans).then(|| ans.to_string())
}

fn preset_grok() -> (String, Provider) {
    (
        "grok".into(),
        Provider {
            kind: "openai".into(),
            host: "https://api.x.ai/v1".into(),
            model: "grok-4.5".into(),
            models: Vec::new(),
            auth: "device".into(),
            api_key_env: "XAI_API_KEY".into(),
        },
    )
}

fn preset_codex() -> (String, Provider) {
    (
        "codex".into(),
        Provider {
            kind: "openai".into(),
            host: "https://chatgpt.com/backend-api/codex".into(),
            model: "gpt-5.3-codex".into(),
            models: Vec::new(),
            auth: "device".into(),
            api_key_env: "OPENAI_API_KEY".into(),
        },
    )
}

fn preset_openrouter() -> (String, Provider) {
    (
        "openrouter".into(),
        Provider {
            kind: "openai".into(),
            host: "https://openrouter.ai/api/v1".into(),
            model: String::new(),
            models: Vec::new(),
            auth: "api_key".into(),
            api_key_env: "OPENROUTER_API_KEY".into(),
        },
    )
}

fn paste_key(
    input: &mut impl BufRead,
    out: &mut impl Write,
    id: &str,
    env_name: &str,
) -> anyhow::Result<()> {
    if !env_name.is_empty() {
        writeln!(out, "paste API key (empty = use ${env_name})")?;
    } else {
        writeln!(out, "paste API key")?;
    }
    let key = prompt(input, out, "> ")?;
    if key.is_empty() {
        if env_name.is_empty()
            || std::env::var(env_name)
                .map(|v| v.is_empty())
                .unwrap_or(true)
        {
            anyhow::bail!("no API key");
        }
        return Ok(());
    }
    auth::set_api_key(id, key)
}

async fn device_login(id: &str) -> anyhow::Result<()> {
    let pending = if id == "codex" {
        auth::request_codex_device().await?
    } else {
        auth::request_device().await?
    };
    println!("Open this URL:\n  {}", pending.verification_uri);
    println!("Code: {}", pending.user_code);
    println!("Waiting for approval (ctrl+c to cancel)…");
    auth::open_url(&pending.verification_uri);
    let cancel = AtomicBool::new(false);
    tokio::select! {
        r = async {
            if id == "codex" {
                auth::complete_codex_device(id, pending, &cancel).await
            } else {
                auth::complete_device(id, pending, &cancel).await
            }
        } => r?,
        _ = tokio::signal::ctrl_c() => anyhow::bail!("cancelled"),
    }
    println!("signed in");
    Ok(())
}

fn prompt(input: &mut impl BufRead, out: &mut impl Write, label: &str) -> anyhow::Result<String> {
    write!(out, "{label}")?;
    out.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_marks_active() {
        let cfg = Config::default();
        let lines = list_lines(&cfg);
        assert!(lines.iter().any(|l| l.starts_with("* local")), "{lines:?}");
    }

    #[test]
    fn pick_by_number_or_name() {
        let cfg = Config::default();
        assert_eq!(pick_id(&cfg, "1").as_deref(), Some("local"));
        assert_eq!(pick_id(&cfg, "local").as_deref(), Some("local"));
        assert_eq!(pick_id(&cfg, "nope"), None);
    }
}
