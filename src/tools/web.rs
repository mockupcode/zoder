use serde_json::{json, Value};

use crate::session::Session;

use super::{arg_path, arg_str, arg_u64};

pub async fn search(args: &Value) -> (bool, String) {
    let Some(query) = arg_str(args, "query") else {
        return (false, "query is required".into());
    };
    let max = arg_u64(args, "count").unwrap_or(10).clamp(1, 20) as usize;
    let url = format!(
        "https://lite.duckduckgo.com/lite/?q={}",
        urlencoding_lite(query)
    );
    let client = match reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; zoder/0.1)")
        .timeout(std::time::Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, e.to_string()),
    };
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => return (false, e.to_string()),
    };
    if resp.status().as_u16() == 202 {
        return (
            false,
            "DuckDuckGo is rate-limiting this machine. Wait a few minutes or fetch known URLs directly."
                .into(),
        );
    }
    let body = match resp.text().await {
        Ok(t) => t,
        Err(e) => return (false, e.to_string()),
    };
    if body.contains("anomaly-modal") || body.contains("Unfortunately, bots use DuckDuckGo too") {
        return (
            false,
            "DuckDuckGo is rate-limiting this machine. Wait a few minutes or fetch known URLs directly."
                .into(),
        );
    }
    let mut results = Vec::new();
    let mut rest = body.as_str();
    while results.len() < max {
        let Some(i) = rest.find("uddg=") else {
            break;
        };
        rest = &rest[i + 5..];
        let enc = rest.split(['&', '"', '\'']).next().unwrap_or("");
        let link = urlencoding_decode(enc);
        let title = rest
            .find('>')
            .and_then(|j| {
                rest[j + 1..]
                    .find('<')
                    .map(|k| rest[j + 1..j + 1 + k].trim())
            })
            .unwrap_or("")
            .to_string();
        if link.starts_with("http") {
            results.push((title, link));
        }
    }
    if results.is_empty() {
        return (true, "No results found. Try rephrasing your search.".into());
    }
    let mut out = format!("Found {} search results:\n\n", results.len());
    for (i, (title, link)) in results.iter().enumerate() {
        out.push_str(&format!("{}. {title}\n   URL: {link}\n\n", i + 1));
    }
    (true, out)
}

pub async fn sourcegraph(args: &Value) -> (bool, String) {
    let Some(query) = arg_str(args, "query") else {
        return (false, "query is required".into());
    };
    let count = arg_u64(args, "count").unwrap_or(10).clamp(1, 20) as usize;
    let gql = json!({
        "query": "query Search($query: String!) { search(query: $query, version: V2, patternType: keyword ) { results { matchCount, limitHit, resultCount, results { __typename, ... on FileMatch { repository { name }, file { path, url, content }, lineMatches { preview, lineNumber } } } } } }",
        "variables": { "query": query }
    });
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, e.to_string()),
    };
    let resp = match client
        .post("https://sourcegraph.com/.api/graphql")
        .header("Content-Type", "application/json")
        .header("User-Agent", "zoder/0.1")
        .json(&gql)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return (false, e.to_string()),
    };
    if !resp.status().is_success() {
        let t = resp.text().await.unwrap_or_default();
        return (false, format!("Request failed: {t}"));
    }
    let v: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return (false, e.to_string()),
    };
    if let Some(errs) = v.get("errors").and_then(|e| e.as_array()) {
        if !errs.is_empty() {
            let msg = errs
                .iter()
                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            return (false, format!("Sourcegraph API error:\n{msg}"));
        }
    }
    let results = v
        .pointer("/data/search/results")
        .cloned()
        .unwrap_or(Value::Null);
    let match_count = results
        .get("matchCount")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    let hits = results.get("results").and_then(|r| r.as_array());
    let Some(hits) = hits else {
        return (true, "No results found. Try a different query.".into());
    };
    let mut out = format!("# Sourcegraph Search Results\n\nFound {match_count} matches\n\n");
    for (i, hit) in hits.iter().take(count).enumerate() {
        if hit.get("__typename").and_then(|t| t.as_str()) != Some("FileMatch") {
            continue;
        }
        let repo = hit
            .pointer("/repository/name")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        let path = hit
            .pointer("/file/path")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        let url = hit
            .pointer("/file/url")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        out.push_str(&format!("## Result {}: {repo}/{path}\n\n", i + 1));
        if !url.is_empty() {
            out.push_str(&format!("URL: {url}\n\n"));
        }
        if let Some(lines) = hit.get("lineMatches").and_then(|l| l.as_array()) {
            out.push_str("```\n");
            for lm in lines.iter().take(8) {
                let n = lm.get("lineNumber").and_then(|n| n.as_u64()).unwrap_or(0);
                let p = lm.get("preview").and_then(|s| s.as_str()).unwrap_or("");
                out.push_str(&format!("{n}| {p}\n"));
            }
            out.push_str("```\n\n");
        }
    }
    (true, out)
}

pub async fn web_search(args: &Value) -> (bool, String) {
    let mut a = args.clone();
    if a.get("count").is_none() {
        if let Some(n) = arg_u64(args, "max_results") {
            if let Some(obj) = a.as_object_mut() {
                obj.insert("count".into(), json!(n));
            }
        }
    }
    search(&a).await
}

pub async fn fetch(args: &Value) -> (bool, String) {
    let Some(url) = arg_str(args, "url") else {
        return (false, "url is required".into());
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return (false, "URL must start with http:// or https://".into());
    }
    let format = arg_str(args, "format").unwrap_or("markdown");
    let timeout = arg_u64(args, "timeout").unwrap_or(30).clamp(1, 120);
    match http_get(url, timeout).await {
        Ok((ctype, body)) => {
            let mut content = if ctype.contains("text/html") {
                match format {
                    "html" => body,
                    "text" => html_to_text(&body),
                    _ => html_to_markdown(&body),
                }
            } else {
                body
            };
            if content.len() > 100 * 1024 {
                content.truncate(100 * 1024);
                content.push_str("\n\n[Content truncated to 100KB]");
            }
            (true, content)
        }
        Err(e) => (false, e),
    }
}

pub async fn web_fetch(_session: &Session, args: &Value) -> (bool, String) {
    let Some(url) = arg_str(args, "url") else {
        return (false, "url is required".into());
    };
    match http_get(url, 30).await {
        Ok((ctype, body)) => {
            let content = if ctype.contains("text/html") {
                html_to_markdown(&body)
            } else {
                body
            };
            if content.len() > 50_000 {
                let dir = crate::config::home_dir().join("cache");
                let _ = std::fs::create_dir_all(&dir);
                let path = dir.join("page.md");
                if let Err(e) = std::fs::write(&path, &content) {
                    return (false, e.to_string());
                }
                return (
                    true,
                    format!(
                        "Fetched content from {url} (large page)\n\nContent saved to: {}\n\nUse view and grep to analyze this file.",
                        path.display()
                    ),
                );
            }
            (true, format!("Fetched content from {url}:\n\n{content}"))
        }
        Err(e) => (false, e),
    }
}

pub async fn download(session: &Session, args: &Value) -> (bool, String) {
    let Some(url) = arg_str(args, "url") else {
        return (false, "url is required".into());
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return (false, "URL must start with http:// or https://".into());
    }
    let path = match arg_path(session, args) {
        Ok(p) => p,
        Err(e) => return (false, e),
    };
    let timeout = arg_u64(args, "timeout").unwrap_or(120).clamp(1, 600);
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .user_agent("zoder/0.1")
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, e.to_string()),
    };
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => return (false, e.to_string()),
    };
    if !resp.status().is_success() {
        return (
            false,
            format!("Request failed with status {}", resp.status()),
        );
    }
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => return (false, e.to_string()),
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, &bytes) {
        Ok(()) => {
            let mut msg = format!(
                "Successfully downloaded {} bytes to {}",
                bytes.len(),
                path.display()
            );
            if !ctype.is_empty() {
                msg.push_str(&format!(" (Content-Type: {ctype})"));
            }
            (true, msg)
        }
        Err(e) => (false, e.to_string()),
    }
}

async fn http_get(url: &str, timeout_s: u64) -> Result<(String, String), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_s))
        .user_agent("zoder/0.1")
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!(
            "Request failed with status code: {}",
            resp.status()
        ));
    }
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() > 100 * 1024 {
        let body = String::from_utf8_lossy(&bytes[..100 * 1024]).into_owned();
        return Ok((ctype, body));
    }
    Ok((ctype, String::from_utf8_lossy(&bytes).into_owned()))
}

fn html_to_text(html: &str) -> String {
    strip_tags(html)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn html_to_markdown(html: &str) -> String {
    let mut s = html
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n");
    for tag in ["p", "div", "h1", "h2", "h3", "h4", "li", "tr"] {
        s = s
            .replace(&format!("<{tag}>"), "\n")
            .replace(&format!("</{tag}>"), "\n");
        s = s.replace(&format!("<{tag} "), "\n");
    }
    strip_tags(&s)
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut skip = false;
    let lower = html.to_ascii_lowercase();
    let chars: Vec<char> = html.chars().collect();
    let lchars: Vec<char> = lower.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if !in_tag && lchars.get(i) == Some(&'<') {
            let rest: String = lchars[i..].iter().take(10).collect();
            if rest.starts_with("<script") || rest.starts_with("<style") {
                skip = true;
            }
            if rest.starts_with("</script") || rest.starts_with("</style") {
                skip = false;
            }
            in_tag = true;
        }
        if !in_tag && !skip {
            out.push(chars[i]);
        }
        if in_tag && chars[i] == '>' {
            in_tag = false;
            out.push(' ');
        }
        i += 1;
    }
    html_unescape(&out)
}

fn html_unescape(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn urlencoding_lite(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
