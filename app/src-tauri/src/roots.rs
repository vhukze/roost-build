//! Root 管理器的版本查询、下载与安装
//!
//! 版本来源：GitHub Releases API（实时查询）
//! 下载来源：GitHub 直连 + 国内镜像

use crate::types::*;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

// ------------------------------------------------------------------ 仓库定义

struct RepoDef {
    id: &'static str,
    name: &'static str,
    desc: &'static str,
    license: &'static str,
    kind: &'static str,
    zygisk: bool,
}

const REPOS: &[RepoDef] = &[
    RepoDef {
        id: "magisk",
        name: "Magisk",
        desc: "需对应 ABI/版本的 prepared ramdisk；下载 APK 不代表可启动。内置 Zygisk 与 sepolicy 工具，LSPosed 完整激活仍须管理器核验。",
        license: "GPL-3.0",
        kind: "ready",
        zygisk: true,
    },
    RepoDef {
        id: "ksu",
        name: "KernelSU",
        desc: "基于内核的 Root 方案。需 Linux 内核集成驱动，模拟器环境下需自行编译内核模块。",
        license: "GPL-2.0",
        kind: "needs-kernel",
        zygisk: true,
    },
    RepoDef {
        id: "apatch",
        name: "APatch",
        desc: "内核 patch 与用户态混合方案，支持运行时加载模块。同样需要内核适配。",
        license: "GPL-2.0",
        kind: "needs-kernel",
        zygisk: true,
    },
    RepoDef {
        id: "sukisu",
        name: "SukiSU-Ultra",
        desc: "KernelSU 活跃分支，改进非 GKI 内核兼容性。需要内核驱动支持。",
        license: "GPL-2.0",
        kind: "needs-kernel",
        zygisk: true,
    },
    RepoDef {
        id: "ksu-next",
        name: "KernelSU-Next",
        desc: "KernelSU 分支，增强模块管理与隐藏机制。需要内核驱动支持。",
        license: "GPL-2.0",
        kind: "needs-kernel",
        zygisk: true,
    },
];

fn repo_path(id: &str) -> Option<&'static str> {
    match id {
        "magisk" => Some("topjohnwu/Magisk"),
        "ksu" => Some("tiann/KernelSU"),
        "apatch" => Some("bmax121/APatch"),
        "sukisu" => Some("SukiSU-Ultra/SukiSU-Ultra"),
        "ksu-next" => Some("KernelSU-Next/KernelSU-Next"),
        _ => None,
    }
}

// ------------------------------------------------------------------ 数据模型

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootVersion {
    pub version: String,
    pub size: String,
    pub date: String,
    #[serde(default)]
    pub downloaded: bool,
    /// 该版本的直接下载地址（GitHub 原始 URL）
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub asset_name: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub featured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootManager {
    pub id: String,
    pub name: String,
    pub desc: String,
    pub source: String,
    pub license: String,
    pub kind: String,
    pub zygisk: bool,
    pub versions: Vec<RootVersion>,
    /// 版本查询状态：ok / offline / error:xxx
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub manager: String,
    pub version: String,
    pub downloaded: u64,
    pub total: u64,
    pub percent: f64,
    pub status: String,
    pub message: String,
}

/// 下载源选择
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadSource {
    /// GitHub 直连
    Direct,
    /// 国内镜像
    Mirror,
}

// ------------------------------------------------------------------ 网络

fn build_client() -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .user_agent("Roost/0.1 (Android Container Manager)")
        .connect_timeout(std::time::Duration::from_secs(12))
        .timeout(std::time::Duration::from_secs(60));

    // 允许用户配置代理（环境变量）
    for key in ["ROOST_PROXY", "HTTPS_PROXY", "https_proxy"] {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                if let Ok(p) = reqwest::Proxy::all(&v) {
                    b = b.proxy(p);
                }
                break;
            }
        }
    }

    b.build()
        .map_err(|e| Error::Other(format!("HTTP 客户端失败: {e}")))
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    browser_download_url: String,
}

/// 从 GitHub API 查询版本列表（带缓存）
async fn fetch_versions(client: &reqwest::Client, repo: &str) -> Result<Vec<RootVersion>> {
    let (body, _from_cache) = crate::ghcache::fetch_with_cache(client, repo)
        .await
        .map_err(|e| Error::Other(e))?;

    let releases: Vec<GhRelease> =
        serde_json::from_str(&body).map_err(|e| Error::Other(format!("解析失败: {e}")))?;

    let mut out = Vec::new();

    for r in releases {
        let apk = r.assets.iter().find(|a| a.name.ends_with(".apk"));
        let Some(apk) = apk else { continue };

        let version = r.tag_name.trim_start_matches('v').to_string();
        let featured = {
            let mgr_id = match repo.split('/').last().unwrap_or("") {
                "Magisk" => "magisk",
                "KernelSU" => "ksu",
                "APatch" => "apatch",
                "SukiSU-Ultra" => "sukisu",
                "KernelSU-Next" => "ksu-next",
                other => other,
            };
            crate::catalog::is_featured(mgr_id, &version)
        };
        let size = format!("{:.1} MB", apk.size as f64 / 1024.0 / 1024.0);
        let date = r
            .published_at
            .as_deref()
            .map(|s| s.chars().take(10).collect::<String>())
            .unwrap_or_default();

        out.push(RootVersion {
            version,
            size,
            date,
            downloaded: false,
            url: apk.browser_download_url.clone(),
            asset_name: apk.name.clone(),
            prerelease: r.prerelease,
            featured,
        });
    }

    Ok(out)
}

// ------------------------------------------------------------------ 目录

pub fn root_binaries_dir() -> PathBuf {
    images_dir().join("root-binaries")
}

fn version_dir(manager: &str, version: &str) -> PathBuf {
    root_binaries_dir().join(manager).join(version)
}

fn marker_path(manager: &str, version: &str) -> PathBuf {
    version_dir(manager, version).join(".downloaded")
}

fn cache_dir() -> PathBuf {
    project_root().join(".cache").join("downloads")
}

// ------------------------------------------------------------------ 查询

/// 列出所有管理器（含实时版本）
pub async fn list_managers() -> Vec<RootManager> {
    let client = match build_client() {
        Ok(c) => Some(c),
        Err(_) => None,
    };

    let mut out = Vec::new();

    for def in REPOS {
        let mut versions = Vec::new();
        let mut status = "ok".to_string();

        if let (Some(c), Some(repo)) = (&client, repo_path(def.id)) {
            match fetch_versions(c, repo).await {
                Ok(v) => versions = v,
                Err(e) => status = format!("error:{}", e.to_string().chars().take(60).collect::<String>()),
            }
        } else {
            status = "offline".into();
        }

        // 标记已下载
        for v in &mut versions {
            v.downloaded = marker_path(def.id, &v.version).exists();
        }

        // 补充本地有但 API 未返回的版本
        let dir = root_binaries_dir().join(def.id);
        if dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    if !e.path().is_dir() {
                        continue;
                    }
                    let name = e.file_name().to_string_lossy().to_string();
                    if marker_path(def.id, &name).exists()
                        && !versions.iter().any(|v| v.version == name)
                    {
                        let size = dir_size(&e.path())
                            .map(|s| format!("{:.1} MB", s as f64 / 1024.0 / 1024.0))
                            .unwrap_or_else(|| "—".into());
                        versions.insert(
                            0,
                            RootVersion {
                                version: name,
                                size,
                                date: "本地".into(),
                                downloaded: true,
                                url: String::new(),
                                asset_name: String::new(),
                                prerelease: false,
                                featured: false,
                            },
                        );
                    }
                }
            }
        }

        // 按版本号排序（新在前）
        versions.sort_by(|a, b| {
            let pa = parse_ver(&a.version);
            let pb = parse_ver(&b.version);
            pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
        });

        out.push(RootManager {
            id: def.id.into(),
            name: def.name.into(),
            desc: def.desc.into(),
            source: format!("https://github.com/{}", repo_path(def.id).unwrap_or("")),
            license: def.license.into(),
            kind: def.kind.into(),
            zygisk: def.zygisk,
            versions,
            status,
        });
    }

    // 手动添加
    out.push(RootManager {
        id: "manual".into(),
        name: "手动添加".into(),
        desc: "将任意 Root 方案的文件放入 images/root-binaries/manual/，按 overlay 规范组织后出现在下拉列表。".into(),
        source: "本地目录".into(),
        license: "—".into(),
        kind: "manual".into(),
        zygisk: false,
        versions: vec![],
        status: "ok".into(),
    });

    out
}

pub fn parse_ver_pub(s: &str) -> Vec<u32> {
    parse_ver(s)
}

fn parse_ver(s: &str) -> Vec<u32> {
    s.split(|c| c == '.' || c == '-' || c == '_')
        .filter_map(|p| p.parse::<u32>().ok())
        .collect()
}

fn dir_size(path: &Path) -> Option<u64> {
    if !path.exists() {
        return None;
    }
    let mut total = 0u64;
    for e in walkdir::WalkDir::new(path).into_iter().flatten() {
        if e.file_type().is_file() {
            total += e.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    Some(total)
}

// ------------------------------------------------------------------ 下载

/// 把 GitHub URL 转成镜像 URL
fn to_mirror(url: &str, index: usize) -> String {
    let mirrors = [
        "https://ghfast.top/",
        "https://gh-proxy.com/",
        "https://ghproxy.net/",
        "https://mirror.ghproxy.com/",
    ];
    let base = mirrors[index % mirrors.len()];
    format!("{base}{url}")
}

/// 下载并安装
pub async fn download_version(
    manager: &str,
    version: &str,
    url: &str,
    asset_name: &str,
    source: DownloadSource,
    progress: impl Fn(DownloadProgress) + Clone,
) -> Result<String> {
    if url.is_empty() {
        return Err(Error::Other("该版本没有可用下载地址".into()));
    }

    validate_component(manager)?; validate_component(version)?; validate_component(asset_name)?;
    let cache = cache_dir();
    std::fs::create_dir_all(&cache)?;
    let apk = cache.join(format!("{manager}-{version}-{}", if asset_name.is_empty() { "pkg.apk" } else { asset_name }));

    let client = build_client()?;

    // 组装候选 URL 列表
    let candidates: Vec<(String, &str)> = match source {
        DownloadSource::Direct => vec![(url.to_string(), "GitHub")],
        DownloadSource::Mirror => vec![
            (to_mirror(url, 0), "镜像 1"),
            (to_mirror(url, 1), "镜像 2"),
            (to_mirror(url, 2), "镜像 3"),
        ],
    };

    let total_c = candidates.len();
    let mut last_err = String::new();
    let mut ok = false;

    for (idx, (u, label)) in candidates.iter().enumerate() {
        progress(DownloadProgress {
            manager: manager.into(),
            version: version.into(),
            downloaded: 0,
            total: 0,
            percent: 0.0,
            status: "downloading".into(),
            message: format!("{label} ({}/{total_c})", idx + 1),
        });

        match fetch_to_file(&client, u, &apk, manager, version, progress.clone()).await {
            Ok(()) => {
                ok = true;
                break;
            }
            Err(e) => {
                last_err = format!("{label}: {e}");
                let _ = std::fs::remove_file(&apk);
            }
        }
    }

    if !ok {
        return Err(Error::Other(format!(
            "下载失败。{last_err}\n\
             提示：直连不通时请改用镜像下载"
        )));
    }

    let size = std::fs::metadata(&apk).map(|m| m.len()).unwrap_or(0);

    // ---- 解包
    progress(DownloadProgress {
        manager: manager.into(),
        version: version.into(),
        downloaded: size,
        total: size,
        percent: 100.0,
        status: "extracting".into(),
        message: "解包中".into(),
    });

    let destination = version_dir(manager, version);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let out = destination.with_file_name(format!(".{version}.staging-{stamp}"));
    let files_dir = out.join("files").join("system").join("bin");
    std::fs::create_dir_all(&files_dir)?;

    let count = extract_binaries(&apk, manager, &files_dir)?;
    if count == 0 {
        let _ = std::fs::remove_file(&apk);
        return Err(Error::Other("APK 中未找到预期二进制".into()));
    }

    // ---- manifest
    let bin = primary_binary(manager);
    let manifest = serde_json::json!({
        "id": format!("{manager}@{version}"),
        "name": format!("{} {}", display_name(manager), version),
        "version": version,
        "description": format!("{} {} (下载安装)", display_name(manager), version),
        "source": "https://github.com",
        "license": "见上游项目",
        "android_min": 8,
        "android_max": 15,
        "requires_kernel_patch": manager != "magisk",
        "zygisk_provider": true,
        "checks": {
            "su_path": format!("/system/bin/{bin}"),
            "verify_cmd": format!("{bin} --version")
        },
        "symlinks": [{ "path": "/system/bin/su", "target": bin }]
    });

    std::fs::write(
        out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    std::fs::copy(&apk, out.join("package.apk"))?;
    extract_abi_resources(&apk, &out)?;
    let digest = crate::managed::sha256_file(&out.join("package.apk"))?;
    std::fs::write(out.join("package.sha256"), format!("{digest}  package.apk\n"))?;
    let previous = destination.with_file_name(format!(".{version}.previous-{stamp}"));
    if destination.exists() { std::fs::rename(&destination, &previous)?; }
    if let Err(error) = std::fs::rename(&out, &destination) {
        if previous.exists() { let _ = std::fs::rename(&previous, &destination); }
        return Err(error.into());
    }
    std::fs::write(marker_path(manager, version), b"ok")?;

    progress(DownloadProgress {
        manager: manager.into(),
        version: version.into(),
        downloaded: size,
        total: size,
        percent: 100.0,
        status: "done".into(),
        message: format!("已安装 {count} 个文件"),
    });

    Ok(format!("{manager} {version} 安装完成，提取 {count} 个二进制"))
}

async fn fetch_to_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    manager: &str,
    version: &str,
    progress: impl Fn(DownloadProgress),
) -> Result<()> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::Other(format!("{e}")))?;

    if !resp.status().is_success() {
        return Err(Error::Other(format!("HTTP {}", resp.status())));
    }

    let total = resp.content_length().unwrap_or(0);
    let mut got = 0u64;

    let mut f = std::fs::File::create(dest)?;
    let mut stream = resp.bytes_stream();

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Error::Other(format!("{e}")))?;
        f.write_all(&chunk)?;
        got += chunk.len() as u64;

        let pct = if total > 0 { got as f64 / total as f64 * 100.0 } else { 0.0 };
        progress(DownloadProgress {
            manager: manager.into(),
            version: version.into(),
            downloaded: got,
            total,
            percent: pct,
            status: "downloading".into(),
            message: format!("{:.1} MB", got as f64 / 1024.0 / 1024.0),
        });
    }
    drop(f);

    let sz = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    if sz < 100 * 1024 {
        return Err(Error::Other(format!("文件过小 ({sz} B)")));
    }
    Ok(())
}

fn display_name(m: &str) -> &'static str {
    match m {
        "magisk" => "Magisk",
        "ksu" => "KernelSU",
        "apatch" => "APatch",
        "sukisu" => "SukiSU-Ultra",
        "ksu-next" => "KernelSU-Next",
        _ => "Root",
    }
}

fn primary_binary(m: &str) -> &'static str {
    match m {
        "magisk" => "magisk",
        "ksu" | "sukisu" | "ksu-next" => "ksud",
        "apatch" => "apd",
        _ => "su",
    }
}

/// 从 APK 提取二进制
fn extract_binaries(apk: &Path, manager: &str, dest: &Path) -> Result<usize> {
    let f = std::fs::File::open(apk)?;
    let mut zip = zip::ZipArchive::new(f)
        .map_err(|e| Error::Other(format!("APK 解析失败: {e}")))?;

    // 每个管理器的 .so -> 目标文件名
    // 允许多个候选名（不同版本命名可能不同）
    let mapping: Vec<(&str, &str)> = match manager {
        "magisk" => vec![
            ("libmagisk.so", "magisk"),
            ("libmagiskpolicy.so", "magiskpolicy"),
            ("libmagiskinit.so", "magiskinit"),
            ("libmagiskboot.so", "magiskboot"),
        ],
        "ksu" | "sukisu" | "ksu-next" => vec![("libksud.so", "ksud")],
        "apatch" => vec![("libapd.so", "apd")],
        _ => vec![],
    };

    let mut count = 0;
    let mut found: Vec<String> = Vec::new();

    for i in 0..zip.len() {
        let mut e = zip
            .by_index(i)
            .map_err(|err| Error::Other(format!("读取失败: {err}")))?;

        let name = e.name().to_string();
        if !name.contains("arm64-v8a") {
            continue;
        }

        let base = Path::new(&name)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        found.push(base.clone());

        for (src, target) in &mapping {
            if base == *src {
                let out = dest.join(target);
                let mut o = std::fs::File::create(&out)?;
                std::io::copy(&mut e, &mut o)?;
                count += 1;
                break;
            }
        }
    }

    if count == 0 && !found.is_empty() {
        // 记录实际找到的库名，便于排查
        let preview = found.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
        return Err(Error::Other(format!(
            "未匹配到预期二进制。APK 内 arm64 库: {preview}"
        )));
    }

    Ok(count)
}

fn extract_abi_resources(apk: &Path, out: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(apk)?)
        .map_err(|e| Error::Other(format!("APK 解析失败: {e}")))?;
    for index in 0..archive.len() {
        let mut member = archive.by_index(index).map_err(|e| Error::Other(e.to_string()))?;
        let Some(relative) = member.enclosed_name().map(|p| p.to_path_buf()) else { continue };
        let name = relative.to_string_lossy().replace('\\', "/");
        if member.is_dir() || !(name.starts_with("lib/") || name.starts_with("assets/")) { continue; }
        let dest = out.join("resources").join(relative);
        if let Some(parent) = dest.parent() { std::fs::create_dir_all(parent)?; }
        let mut file = std::fs::File::create(dest)?;
        std::io::copy(&mut member, &mut file)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ 删除

pub fn remove_version(manager: &str, version: &str) -> Result<()> {
    validate_component(manager)?; validate_component(version)?;
    let dir = version_dir(manager, version);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

pub fn is_downloaded(manager: &str, version: &str) -> bool {
    if validate_component(manager).is_err() || validate_component(version).is_err() { return false; }
    marker_path(manager, version).exists()
}
