//! LSPosed 版本管理与 Magisk/LSPosed 模块仓库

use crate::types::*;
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------ 数据源

/// LSPosed 官方仓库
pub const LSPOSED_REPO: &str = "LSPosed/LSPosed";
/// LSPosed 模块仓库组织（modules.lsposed.org 的后端）
pub const LSPOSED_MODULE_ORG: &str = "Xposed-Modules-Repo";
/// Magisk 模块仓库组织
pub const MAGISK_MODULE_ORG: &str = "Magisk-Modules-Repo";

// ------------------------------------------------------------------ 通用模型

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgVersion {
    pub version: String,
    pub variant: String,
    pub size: String,
    pub date: String,
    pub url: String,
    pub asset_name: String,
    pub downloaded: bool,
    pub featured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgManager {
    pub id: String,
    pub name: String,
    pub desc: String,
    pub source: String,
    pub versions: Vec<PkgVersion>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleInfo {
    #[serde(default)]
    pub kind: String,
    pub full_name: String,
    pub package: String,
    pub name: String,
    pub description: String,
    pub stars: u32,
    pub updated: String,
    pub html_url: String,
    pub apk_url: String,
    pub apk_name: String,
    pub version: String,
}

// ------------------------------------------------------------------ 目录

pub fn lsposed_dir() -> std::path::PathBuf {
    images_dir().join("lsposed")
}

fn lsposed_ver_dir(version: &str, variant: &str) -> std::path::PathBuf {
    lsposed_dir().join(format!("{version}-{variant}"))
}

fn lsposed_marker(version: &str, variant: &str) -> std::path::PathBuf {
    lsposed_ver_dir(version, variant).join(".downloaded")
}

pub fn modules_cache_dir() -> std::path::PathBuf {
    images_dir().join("modules")
}

// ------------------------------------------------------------------ 网络

fn client() -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Roost/0.1")
        .connect_timeout(std::time::Duration::from_secs(12))
        .timeout(std::time::Duration::from_secs(120));

    if let Some(proxy) = crate::proxy::detect() {
        if let Ok(p) = reqwest::Proxy::all(&proxy) {
            b = b.proxy(p);
        }
    }
    b.build().map_err(|e| Error::Other(format!("{e}")))
}

fn to_mirror(url: &str, i: usize) -> String {
    let m = [
        "https://ghfast.top/",
        "https://gh-proxy.com/",
        "https://ghproxy.net/",
    ];
    format!("{}{}", m[i % m.len()], url)
}

// ------------------------------------------------------------------ LSPosed 版本

#[derive(Deserialize)]
struct GhRel {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GhAs>,
}

#[derive(Deserialize)]
struct GhAs {
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    browser_download_url: String,
}

/// 查询 LSPosed 版本列表
///
/// 数据来自两个源并合并：
///   1. lsposed.zip/update.json —— 官方新分发点，只提供最新版
///   2. GitHub Releases —— v1.9.2 及更早的历史版本
pub async fn list_lsposed() -> PkgManager {
    let mut versions: Vec<PkgVersion> = Vec::new();
    let mut status = "ok".to_string();

    let c = match client() {
        Ok(c) => c,
        Err(e) => {
            return PkgManager {
                id: "lsposed".into(),
                name: "LSPosed".into(),
                desc: LSPOSED_DESC.into(),
                source: "https://lsposed.zip".into(),
                versions: vec![],
                status: format!("error:{e}"),
            }
        }
    };

    // ---- 源 1：lsposed.zip 最新版
    let mut latest_ver = String::new();
    if let Ok(resp) = c
        .get("https://lsposed.zip/update.json")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    let ver = v
                        .get("version")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim_start_matches('v')
                        .to_string();
                    let code = v.get("versionCode").and_then(|x| x.as_u64()).unwrap_or(0);
                    let zip_url = v
                        .get("zipUrl")
                        .and_then(|x| x.as_str())
                        .unwrap_or("https://lsposed.zip");

                    if !ver.is_empty() {
                        latest_ver = ver.clone();

                        // HEAD 请求取文件大小
                        let size = head_size(&c, zip_url).await;
                        let size_str = if size > 0 {
                            format!("{:.1} MB", size as f64 / 1024.0 / 1024.0)
                        } else {
                            "6.4 MB".into()
                        };

                        versions.push(PkgVersion {
                            version: ver.clone(),
                            variant: "unified".into(),
                            size: size_str,
                            date: format!("build {code}"),
                            url: zip_url.to_string(),
                            asset_name: format!("LSPosed-v{ver}.zip"),
                            downloaded: lsposed_marker(&ver, "unified").exists(),
                            featured: false,
                        });
                    }
                }
            }
        }
    }

    // ---- 源 2：GitHub Releases（历史版本）
    match crate::ghcache::fetch_with_cache(&c, LSPOSED_REPO).await {
        Ok((body, _)) => {
            if let Ok(rels) = serde_json::from_str::<Vec<GhRel>>(&body) {
                for r in rels {
                    let ver = r.tag_name.trim_start_matches('v').to_string();
                    // Keep historical zygisk/riru variants even if unified has this version.
                    let date = r
                        .published_at
                        .as_deref()
                        .map(|s| s.chars().take(10).collect::<String>())
                        .unwrap_or_default();

                    for a in &r.assets {
                        if !a.name.ends_with(".zip") {
                            continue;
                        }
                        let variant = if a.name.contains("zygisk") {
                            "zygisk"
                        } else if a.name.contains("riru") {
                            "riru"
                        } else {
                            "other"
                        };

                        versions.push(PkgVersion {
                            version: ver.clone(),
                            variant: variant.into(),
                            size: format!("{:.1} MB", a.size as f64 / 1024.0 / 1024.0),
                            date: date.clone(),
                            url: a.browser_download_url.clone(),
                            asset_name: a.name.clone(),
                            downloaded: lsposed_marker(&ver, variant).exists(),
                            featured: crate::catalog::lsposed_featured()
                                .contains(&ver.as_str()),
                        });
                    }
                }
            }
        }
        Err(e) => {
            if latest_ver.is_empty() {
                status = format!("error:{}", e.chars().take(50).collect::<String>());
            }
        }
    }

    // ---- 源 3：Telegram 频道（版本历史，需代理）
    let tg_versions = fetch_tg_versions(&c).await;
    for (ver, size, msg_url, date) in &tg_versions {
        // 已存在则跳过（以 lsposed.zip 或 GitHub 的下载地址优先）
        if versions.iter().any(|v| v.version == *ver) {
            continue;
        }
        versions.push(PkgVersion {
            version: ver.clone(),
            variant: "tg".into(),
            size: size.clone(),
            date: date.clone(),
            // TG 文件需客户端鉴权，这里给消息页地址供手动下载
            url: msg_url.clone(),
            asset_name: format!("LSPosed-v{ver}.zip"),
            downloaded: lsposed_marker(ver, "unified").exists(),
            featured: false,
        });
    }

    // 排序：最新版置顶，其余按版本号降序
    versions.sort_by(|a, b| {
        let a_latest = a.version == latest_ver;
        let b_latest = b.version == latest_ver;
        if a_latest != b_latest {
            return if a_latest {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        let pa = crate::roots::parse_ver_pub(&a.version);
        let pb = crate::roots::parse_ver_pub(&b.version);
        pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
    });

    PkgManager {
        id: "lsposed".into(),
        name: "LSPosed".into(),
        desc: LSPOSED_DESC.into(),
        source: "https://lsposed.zip".into(),
        versions,
        status,
    }
}

const LSPOSED_DESC: &str =
    "Xposed 框架的现代实现。最新版由 lsposed.zip 分发（支持 Android 17 / riscv64），历史版本来自 GitHub Releases 与 Telegram 频道。";

/// TG 频道
const LSPOSED_TG: &str = "LSPosed";

/// 从 TG 频道预览页解析版本历史
///
/// 页面中每个文件消息形如：
///   <a class="tgme_widget_message_document_wrap" href="https://t.me/LSPosed/317">
///     <div class="...document_title...">LSPosed-v2.0.4-7741-release.zip</div>
///     <div class="...document_extra...">8.4 MB</div>
///   </a>
async fn fetch_tg_versions(c: &reqwest::Client) -> Vec<(String, String, String, String)> {
    // 返回 (version, size, tg_message_url, date)
    let url = format!("https://t.me/s/{LSPOSED_TG}");
    let resp = match c.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return vec![],
    };
    let html = match resp.text().await {
        Ok(t) => t,
        Err(_) => return vec![],
    };

    let mut out: Vec<(String, String, String, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // ---- 1. 附件形式（可直接看到文件名与大小）
    let chunks: Vec<&str> = html.split("tgme_widget_message_document_wrap").collect();
    for chunk in chunks.iter().skip(1) {
        let msg_url = extract_attr(chunk, "href=\"https://t.me/").unwrap_or_default();
        let fname = extract_between(
            chunk,
            "document_title accent_color\" dir=\"auto\">",
            "</div>",
        )
        .unwrap_or_default();
        let size = extract_between(chunk, "document_extra\" dir=\"auto\">", "</div>")
            .unwrap_or_default();
        let date = extract_between(chunk, "<time datetime=\"", "T").unwrap_or_default();

        if !fname.to_lowercase().ends_with(".zip") || !fname.contains("LSPosed") {
            continue;
        }

        let version = fname
            .trim_start_matches("LSPosed-v")
            .trim_start_matches("LSPosed-")
            .split('-')
            .next()
            .unwrap_or("")
            .to_string();

        if version.is_empty() || !seen.insert(version.clone()) {
            continue;
        }

        let full_url = if msg_url.starts_with("https://") {
            msg_url
        } else {
            format!("https://t.me/{msg_url}")
        };

        out.push((version, size, full_url, date));
    }

    // ---- 2. 文本公告形式（版本号写在正文里）
    // 匹配形如 "v2.0.3 released" / "v2.1.0" / "v2.1.1" 的标题
    let text_chunks: Vec<&str> = html
        .split("tgme_widget_message_text js-message_text")
        .collect();

    for chunk in text_chunks.iter().skip(1) {
        // 取该消息块前 400 字符，版本号通常出现在开头
        let head: String = chunk.chars().take(400).collect();

        // 消息链接（该块内第一个 t.me/LSPosed/NNN）
        let msg_url = extract_attr(chunk, "href=\"https://t.me/")
            .unwrap_or_else(|| format!("https://t.me/{LSPOSED_TG}"));

        // 日期
        let date = extract_between(chunk, "<time datetime=\"", "T").unwrap_or_default();

        // 扫描 vX.Y.Z 形式
        let bytes = head.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'v' || bytes[i] == b'V' {
                // 尝试解析 v数字.数字.数字
                if let Some((ver, len)) = parse_semver(&head[i..]) {
                    if !seen.contains(&ver) && is_plausible_version(&ver) {
                        seen.insert(ver.clone());
                        out.push((ver, "—".into(), msg_url.clone(), date.clone()));
                    }
                    i += len;
                    continue;
                }
            }
            i += 1;
        }
    }

    out
}

/// 从 "v2.0.3 ..." 解析出版本号
fn parse_semver(s: &str) -> Option<(String, usize)> {
    let mut chars = s.char_indices();
    let (_, first) = chars.next()?;
    if first != 'v' && first != 'V' {
        return None;
    }

    let mut out = String::new();
    let mut dots = 0;
    let mut end = 1;

    for (idx, ch) in s.char_indices().skip(1) {
        match ch {
            '0'..='9' => {
                out.push(ch);
                end = idx + 1;
            }
            '.' if dots < 2 => {
                out.push(ch);
                dots += 1;
                end = idx + 1;
            }
            _ => break,
        }
    }

    if dots >= 1 && !out.is_empty() {
        Some((out, end))
    } else {
        None
    }
}

/// 过滤掉明显不是 LSPosed 版本的号（如 Android 版本号）
fn is_plausible_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() < 2 {
        return false;
    }
    let major: u32 = parts[0].parse().unwrap_or(999);
    // LSPosed 当前主版本是 1 或 2，排除 9/10/11/12/13/14/15/16/17 这类 Android 版本
    major <= 5
}

fn extract_between(s: &str, start: &str, end: &str) -> Option<String> {
    let i = s.find(start)? + start.len();
    let rest = &s[i..];
    let j = rest.find(end)?;
    Some(rest[..j].trim().to_string())
}

/// 从 HTML 片段中提取第一个 https:// 开头的链接
fn extract_attr(s: &str, marker: &str) -> Option<String> {
    let i = s.find(marker)?;
    let rest = &s[i..];
    // 定位到 URL 起始
    let url_start = rest.find("https://")?;
    let after = &rest[url_start..];
    let j = after.find('"')?;
    let url = after[..j].trim();

    // 过滤掉明显不是消息链接的（如 t.me/s/ 、cdn 图片等）
    if url.contains("t.me/s/") || url.contains("cdn") {
        return None;
    }
    Some(url.to_string())
}

/// 用 HEAD 请求获取文件大小
async fn head_size(c: &reqwest::Client, url: &str) -> u64 {
    if let Ok(r) = c
        .head(url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        return r.content_length().unwrap_or(0);
    }
    0
}

/// 下载 LSPosed（zip 直接保存，安装时由 Root 引擎推送）
pub async fn download_lsposed(
    version: &str,
    variant: &str,
    url: &str,
    asset_name: &str,
    use_mirror: bool,
    progress: impl Fn(crate::roots::DownloadProgress) + Clone,
) -> Result<String> {
    validate_component(version)?; validate_component(variant)?; validate_component(asset_name)?;
    let dir = lsposed_ver_dir(version, variant);
    std::fs::create_dir_all(&dir)?;

    let dest = dir.join(if asset_name.is_empty() {
        "lsposed.zip"
    } else {
        asset_name
    });

    let c = client()?;
    let candidates: Vec<(String, &str)> = if use_mirror {
        vec![
            (to_mirror(url, 0), "镜像 1"),
            (to_mirror(url, 1), "镜像 2"),
        ]
    } else {
        vec![(url.to_string(), "GitHub")]
    };

    let mut last = String::new();
    let mut ok = false;

    for (u, label) in &candidates {
        progress(crate::roots::DownloadProgress {
            manager: "lsposed".into(),
            version: version.into(),
            downloaded: 0,
            total: 0,
            percent: 0.0,
            status: "downloading".into(),
            message: format!("{label}"),
        });

        match download_to(&c, u, &dest, version, progress.clone()).await {
            Ok(()) => {
                ok = true;
                break;
            }
            Err(e) => last = format!("{label}: {e}"),
        }
    }

    if !ok {
        return Err(Error::Other(format!("下载失败: {last}")));
    }

    std::fs::write(lsposed_marker(version, variant), b"ok")?;

    progress(crate::roots::DownloadProgress {
        manager: "lsposed".into(),
        version: version.into(),
        downloaded: 0,
        total: 0,
        percent: 100.0,
        status: "done".into(),
        message: "已下载".into(),
    });

    Ok(format!("LSPosed {version} ({variant}) 下载完成"))
}

async fn download_to(
    c: &reqwest::Client,
    url: &str,
    dest: &std::path::Path,
    version: &str,
    progress: impl Fn(crate::roots::DownloadProgress),
) -> Result<()> {
    use futures_util::StreamExt;
    use std::io::Write;

    let r = c.get(url).send().await.map_err(|e| Error::Other(format!("{e}")))?;
    if !r.status().is_success() {
        return Err(Error::Other(format!("HTTP {}", r.status())));
    }

    let total = r.content_length().unwrap_or(0);
    let mut got = 0u64;
    let mut f = std::fs::File::create(dest)?;
    let mut s = r.bytes_stream();

    while let Some(ch) = s.next().await {
        let ch = ch.map_err(|e| Error::Other(format!("{e}")))?;
        f.write_all(&ch)?;
        got += ch.len() as u64;
        let pct = if total > 0 { got as f64 / total as f64 * 100.0 } else { 0.0 };
        progress(crate::roots::DownloadProgress {
            manager: "lsposed".into(),
            version: version.into(),
            downloaded: got,
            total,
            percent: pct,
            status: "downloading".into(),
            message: format!("{:.1} MB", got as f64 / 1024.0 / 1024.0),
        });
    }
    drop(f);

    if std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0) < 100 * 1024 {
        return Err(Error::Other("文件过小".into()));
    }
    Ok(())
}

/// 删除 LSPosed 版本
pub fn remove_lsposed(version: &str, variant: &str) -> Result<()> {
    validate_component(version)?; validate_component(variant)?;
    let d = lsposed_ver_dir(version, variant);
    if d.exists() {
        std::fs::remove_dir_all(d)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ 模块搜索

/// 搜索模块（org = lsposed | magisk）
pub async fn search_modules(org: &str, query: &str, page: u32) -> Result<Vec<ModuleInfo>> {
    let c = client()?;

    let base_org = match org {
        "magisk" => MAGISK_MODULE_ORG,
        _ => LSPOSED_MODULE_ORG,
    };

    let body = if query.trim().is_empty() {
        // 无关键词：列出组织仓库
        let url = format!(
            "https://api.github.com/orgs/{base_org}/repos?per_page=50&sort=updated&page={page}"
        );
        fetch_json(&c, &url).await?
    } else {
        // 有关键词：搜索组织内仓库
        let q = format!("{} org:{}", query.trim(), base_org);
        let url = format!(
            "https://api.github.com/search/repositories?q={}&per_page=50&page={page}",
            urlencode(&q)
        );
        let raw = fetch_json(&c, &url).await?;
        // 搜索 API 返回 { items: [] }，取出 items 数组
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        let items = v.get("items").cloned().unwrap_or(serde_json::json!([]));
        serde_json::to_string(&items).map_err(|e| Error::Other(format!("{e}")))?
    };

    Ok(parse_repos(&body, base_org))
}

async fn fetch_json(c: &reqwest::Client, url: &str) -> Result<String> {
    // 缓存
    let mut cache = crate::ghcache::read_cache();
    if let Some(e) = cache.get(url) {
        let ts = e.get("ts").and_then(|v| v.as_u64()).unwrap_or(0);
        if crate::ghcache::now_ts().saturating_sub(ts) < 3600 {
            if let Some(b) = e.get("body").and_then(|v| v.as_str()) {
                return Ok(b.to_string());
            }
        }
    }

    let r = c
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| Error::Other(format!("{e}")))?;

    if !r.status().is_success() {
        // 回退缓存
        if let Some(e) = cache.get(url) {
            if let Some(b) = e.get("body").and_then(|v| v.as_str()) {
                return Ok(b.to_string());
            }
        }
        return Err(Error::Other(format!("HTTP {}", r.status())));
    }

    let body = r.text().await.map_err(|e| Error::Other(format!("{e}")))?;
    cache.insert(
        url.to_string(),
        serde_json::json!({ "ts": crate::ghcache::now_ts(), "body": body }),
    );
    crate::ghcache::write_cache(&cache);
    Ok(body)
}

fn parse_repos(body: &str, org: &str) -> Vec<ModuleInfo> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        full_name: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        stargazers_count: u32,
        #[serde(default)]
        updated_at: String,
        #[serde(default)]
        html_url: String,
    }

    let arr: Vec<R> = serde_json::from_str(body).unwrap_or_default();
    arr.into_iter()
        .filter(|r| r.full_name.starts_with(&format!("{org}/")))
        .map(|r| {
            let pkg = r
                .full_name
                .strip_prefix(&format!("{org}/"))
                .unwrap_or(&r.full_name)
                .to_string();
            ModuleInfo {
                kind: if org == MAGISK_MODULE_ORG { "magisk_zip".into() } else { "apk".into() },
                package: pkg.clone(),
                name: pkg,
                description: r.description.unwrap_or_default(),
                stars: r.stargazers_count,
                updated: r.updated_at.chars().take(10).collect(),
                html_url: r.html_url,
                full_name: r.full_name,
                apk_url: String::new(),
                apk_name: String::new(),
                version: String::new(),
            }
        })
        .collect()
}

/// 查询模块的最新 release（获取 APK 地址）
pub async fn module_release(full_name: &str, kind: &str) -> Result<ModuleInfo> {
    if kind != "apk" && kind != "magisk_zip" { return Err(Error::Other("未知模块类型".into())); }
    let parts: Vec<&str> = full_name.split('/').collect();
    if parts.len() != 2 { return Err(Error::Other("仓库名格式无效".into())); }
    for part in parts { validate_component(part)?; }
    let c = client()?;
    let url = format!("https://api.github.com/repos/{full_name}/releases/latest");
    let body = fetch_json(&c, &url).await?;

    let v: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| Error::Other(format!("{e}")))?;

    let tag = v.get("tag_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let assets = v.get("assets").and_then(|x| x.as_array()).cloned().unwrap_or_default();

    let apk = assets.iter().find(|a| {
        a.get("name")
            .and_then(|n| n.as_str())
            .map(|n| n.to_lowercase().ends_with(if kind == "magisk_zip" { ".zip" } else { ".apk" }))
            .unwrap_or(false)
    });

    let (apk_url, apk_name, size) = match apk {
        Some(a) => (
            a.get("browser_download_url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            a.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            a.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
        ),
        None => (String::new(), String::new(), 0),
    };

    if apk_url.is_empty() {
        return Err(Error::Other("该模块没有所选类型的发布资产".into()));
    }

    Ok(ModuleInfo {
        kind: kind.into(),
        full_name: full_name.to_string(),
        package: full_name.split('/').last().unwrap_or("").to_string(),
        name: full_name.split('/').last().unwrap_or("").to_string(),
        description: String::new(),
        stars: 0,
        updated: String::new(),
        html_url: format!("https://github.com/{full_name}"),
        apk_url,
        apk_name,
        version: format!("{tag} ({:.1} MB)", size as f64 / 1024.0 / 1024.0),
    })
}

/// 下载模块 APK 到本地缓存
pub async fn download_module(
    full_name: &str,
    apk_url: &str,
    apk_name: &str,
    use_mirror: bool,
) -> Result<String> {
    let parts: Vec<&str> = full_name.split('/').collect();
    if parts.len() != 2 { return Err(Error::Other("仓库名格式无效".into())); }
    for part in parts { validate_component(part)?; }
    validate_component(apk_name)?;
    let dir = modules_cache_dir();
    std::fs::create_dir_all(&dir)?;

    let safe = full_name.replace('/', "__");
    let dest = dir.join(format!("{safe}__{}", if apk_name.is_empty() { "module.apk" } else { apk_name }));

    let c = client()?;
    let candidates: Vec<String> = if use_mirror {
        vec![to_mirror(apk_url, 0), to_mirror(apk_url, 1)]
    } else {
        vec![apk_url.to_string()]
    };

    let mut last = String::new();
    for u in &candidates {
        match raw_download(&c, u, &dest).await {
            Ok(()) => {
                return Ok(dest.to_string_lossy().to_string());
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(Error::Other(format!("下载失败: {last}")))
}

async fn raw_download(c: &reqwest::Client, url: &str, dest: &std::path::Path) -> Result<()> {
    use futures_util::StreamExt;
    use std::io::Write;

    let r = c.get(url).send().await.map_err(|e| Error::Other(format!("{e}")))?;
    if !r.status().is_success() {
        return Err(Error::Other(format!("HTTP {}", r.status())));
    }

    let mut f = std::fs::File::create(dest)?;
    let mut s = r.bytes_stream();
    while let Some(ch) = s.next().await {
        let ch = ch.map_err(|e| Error::Other(format!("{e}")))?;
        f.write_all(&ch)?;
    }
    drop(f);

    if std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0) < 10 * 1024 {
        return Err(Error::Other("文件过小".into()));
    }
    Ok(())
}

fn urlencode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                o.push(b as char)
            }
            b' ' => o.push_str("%20"),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}
