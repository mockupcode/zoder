use std::path::{Path, PathBuf};

use anyhow::Context;

const REPO: &str = "mockupcode/zoder";
const USAGE: &str = "\
zoder update [version]
";

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(v) if v.starts_with('-') => anyhow::bail!("unknown flag `{v}`\n{USAGE}"),
        Some(v) => install(Some(v.trim_start_matches('v'))).await,
        None => install(None).await,
    }
}

async fn install(want: Option<&str>) -> anyhow::Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let (version, url) = match want {
        Some(v) => {
            let version = v.to_string();
            let url = asset_url(&version)?;
            (version, url)
        }
        None => {
            let latest = latest_tag().await?;
            if version_tuple(&latest) <= version_tuple(current) {
                println!("zoder {current} is up to date");
                return Ok(());
            }
            let url = asset_url(&latest)?;
            (latest, url)
        }
    };
    if version == current {
        println!("zoder {current} is up to date");
        return Ok(());
    }
    println!("updating {current} → {version}");
    let bytes = download(&url).await?;
    replace_self(&bytes)?;
    println!("installed zoder {version}");
    Ok(())
}

fn asset_name() -> anyhow::Result<&'static str> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("macos", "aarch64") => Ok("zoder-macos-aarch64"),
        ("macos", "x86_64") => Ok("zoder-macos-x86_64"),
        ("linux", "x86_64") => Ok("zoder-linux-x86_64"),
        ("linux", "aarch64") => Ok("zoder-linux-aarch64"),
        _ => anyhow::bail!("no release binary for {os}-{arch}"),
    }
}

fn asset_url(version: &str) -> anyhow::Result<String> {
    let name = asset_name()?;
    Ok(format!(
        "https://github.com/{REPO}/releases/download/v{version}/{name}"
    ))
}

fn http() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(format!("zoder/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()?)
}

async fn latest_tag() -> anyhow::Result<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let json: serde_json::Value = http()?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    parse_tag(&json).context("no latest release tag")
}

pub(crate) fn parse_tag(json: &serde_json::Value) -> Option<String> {
    json.get("tag_name")
        .and_then(|t| t.as_str())
        .map(|t| t.trim_start_matches('v').to_string())
        .filter(|t| !t.is_empty())
}

fn version_tuple(v: &str) -> (u32, u32, u32) {
    let v = v.trim_start_matches('v');
    let mut it = v.split('.');
    let major = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor, patch)
}

async fn download(url: &str) -> anyhow::Result<Vec<u8>> {
    let res = http()?.get(url).send().await?.error_for_status()?;
    Ok(res.bytes().await?.to_vec())
}

fn replace_self(bytes: &[u8]) -> anyhow::Result<()> {
    let dest = std::env::current_exe().context("current executable")?;
    let dest = dest.canonicalize().unwrap_or(dest);
    write_atomic(&dest, bytes)
}

fn write_atomic(dest: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = PathBuf::from(format!("{}.update-{}", dest.display(), std::process::id()));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&tmp)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&tmp, perm)?;
    }
    std::fs::rename(&tmp, dest)
        .or_else(|_| {
            std::fs::copy(&tmp, dest).map(|_| ())?;
            std::fs::remove_file(&tmp)
        })
        .with_context(|| format!("replacing {}", dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_tag() {
        let v = serde_json::json!({ "tag_name": "v0.1.1" });
        assert_eq!(parse_tag(&v).as_deref(), Some("0.1.1"));
    }

    #[test]
    fn newer_tag_sorts_after_current() {
        assert!(version_tuple("0.1.1") > version_tuple("0.1.0"));
        assert_eq!(version_tuple("v0.1.1"), version_tuple("0.1.1"));
    }
}
