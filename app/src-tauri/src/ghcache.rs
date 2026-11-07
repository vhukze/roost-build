//! GitHub API 代理与缓存
//!
//! GitHub 匿名 API 限速 60 次/小时，需要：
//!   1. 优先走镜像/加速 API
//!   2. 本地缓存结果（24 小时）
//!   3. 失败时回退到缓存

use std::path::PathBuf;

fn cache_file() -> PathBuf {
    crate::types::project_root().join(".cache").join("gh-releases.json")
}

/// 缓存结构：{ repo: { ts: u64, body: String } }
pub fn read_cache() -> serde_json::Map<String, serde_json::Value> {
    let p = cache_file();
    if !p.exists() {
        return serde_json::Map::new();
    }
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn write_cache(map: &serde_json::Map<String, serde_json::Value>) {
    let p = cache_file();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(map) {
        let _ = std::fs::write(p, text);
    }
}

pub fn now_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 带缓存的获取
pub async fn fetch_with_cache(
    client: &reqwest::Client,
    repo: &str,
) -> Result<(String, bool), String> {
    let mut cache = read_cache();
    let ttl = 24 * 3600u64; // 24 小时
    let now = now_ts();

    // 1. 缓存命中
    if let Some(entry) = cache.get(repo) {
        let ts = entry.get("ts").and_then(|v| v.as_u64()).unwrap_or(0);
        if now.saturating_sub(ts) < ttl {
            if let Some(body) = entry.get("body").and_then(|v| v.as_str()) {
                return Ok((body.to_string(), true));
            }
        }
    }

    // 2. 网络获取（多源尝试）
    let path = format!("repos/{repo}/releases?per_page=20");
    let candidates = [
        format!("https://api.github.com/{path}"),
        format!("https://gh-proxy.com/https://api.github.com/{path}"),
        format!("https://ghfast.top/https://api.github.com/{path}"),
    ];

    let mut last_err = String::new();
    for url in &candidates {
        match client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                match r.text().await {
                    Ok(body) => {
                        // 校验是合法 JSON 数组
                        if body.trim_start().starts_with('[') {
                            cache.insert(
                                repo.to_string(),
                                serde_json::json!({ "ts": now, "body": body }),
                            );
                            write_cache(&cache);
                            return Ok((body, false));
                        }
                        last_err = "返回内容不是数组".into();
                    }
                    Err(e) => last_err = format!("读取失败: {e}"),
                }
            }
            Ok(r) => last_err = format!("HTTP {}", r.status()),
            Err(e) => last_err = format!("{e}"),
        }
    }

    // 3. 全部失败时回退到过期缓存
    if let Some(entry) = cache.get(repo) {
        if let Some(body) = entry.get("body").and_then(|v| v.as_str()) {
            return Ok((body.to_string(), true));
        }
    }

    Err(last_err)
}
