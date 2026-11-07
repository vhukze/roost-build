//! 云端产物清单
//!
//! 终端用户无需编译：App 读取此清单，直接下载已编译好的内核等产物。
//!
//! 清单托管在 roost-build 仓库，内核由 GitHub Actions 编译后自动写入 URL。

use crate::types::*;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

// ------------------------------------------------------------------ 清单结构

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kernel_version: String,
    #[serde(default)]
    pub defconfig: String,
    #[serde(default)]
    pub api_min: u32,
    #[serde(default)]
    pub api_max: u32,
    #[serde(default)]
    pub has_ksu: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size: String,
    /// ready | not-built
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub build_run: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageEntryLite {
    pub id: String,
    pub name: String,
    pub api: u32,
    #[serde(default)]
    pub abi: String,
    #[serde(default)]
    pub source: String,
    pub url: String,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelUrls {
    #[serde(default)]
    pub primary: String,
    #[serde(default)]
    pub mirror: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ArtifactManifest {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub kernels: Vec<KernelEntry>,
    #[serde(default)]
    pub images: Vec<ImageEntryLite>,
    #[serde(default)]
    pub channels: ChannelUrls,
}

/// 带本地状态的内核条目
#[derive(Debug, Clone, Serialize)]
pub struct KernelStatus {
    #[serde(flatten)]
    pub entry: KernelEntry,
    /// 是否已下载到本地
    pub installed: bool,
    /// 是否正在使用（与 images/base/kernel 的 sha 匹配）
    pub active: bool,
}

// ------------------------------------------------------------------ 目录

const DEFAULT_MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/vhukze/roost-build/main/artifacts.json";
const MIRROR_MANIFEST_URL: &str =
    "https://ghfast.top/https://raw.githubusercontent.com/vhukze/roost-build/main/artifacts.json";

pub fn kernels_dir() -> PathBuf {
    images_dir().join("kernels")
}

fn kernel_marker(id: &str) -> PathBuf {
    kernels_dir().join(format!("{id}.installed"))
}

fn manifest_cache() -> PathBuf {
    images_dir().join("aosp-cache").join("artifacts.json")
}

// ------------------------------------------------------------------ 网络

fn client() -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .user_agent("Roost/0.1")
        .connect_timeout(std::time::Duration::from_secs(12))
        .timeout(std::time::Duration::from_secs(600));

    if let Some(p) = crate::proxy::detect() {
        if let Ok(px) = reqwest::Proxy::all(&p) {
            b = b.proxy(px);
        }
    }
    b.build().map_err(|e| Error::Other(format!("{e}")))
}

// ------------------------------------------------------------------ 清单读取

/// 拉取云端清单（含缓存与镜像回退）
pub async fn fetch_manifest() -> Result<ArtifactManifest> {
    let c = client()?;
    let cache = manifest_cache();

    // 1. 先用缓存（如果有）
    let cached = if cache.exists() {
        std::fs::read_to_string(&cache)
            .ok()
            .and_then(|t| serde_json::from_str::<ArtifactManifest>(&t).ok())
    } else {
        None
    };

    // 2. 尝试网络
    for url in [DEFAULT_MANIFEST_URL, MIRROR_MANIFEST_URL] {
        match c.get(url).send().await {
            Ok(r) if r.status().is_success() => {
                if let Ok(text) = r.text().await {
                    if let Ok(m) = serde_json::from_str::<ArtifactManifest>(&text) {
                        // 写缓存
                        if let Some(parent) = cache.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(&cache, &text);
                        return Ok(m);
                    }
                }
            }
            _ => continue,
        }
    }

    // 3. 网络失败则用缓存
    cached.ok_or_else(|| {
        Error::Other("无法获取产物清单：网络不可达且无本地缓存".into())
    })
}

/// 列出内核并附上本地安装状态
pub async fn list_kernels() -> Result<Vec<KernelStatus>> {
    let m = fetch_manifest().await?;

    // 读取当前生效的内核 sha
    let active_sha = current_kernel_sha();

    Ok(m.kernels
        .into_iter()
        .map(|e| {
            let installed = kernel_marker(&e.id).exists();
            let active = !e.sha256.is_empty() && active_sha.as_deref() == Some(&e.sha256);
            KernelStatus {
                entry: e,
                installed,
                active,
            }
        })
        .collect())
}

fn current_kernel_sha() -> Option<String> {
    let p = base_images_dir().join("kernel");
    if !p.exists() {
        return None;
    }
    // 简单读取文件算 sha256
    let bytes = std::fs::read(&p).ok()?;
    Some(sha256_hex(&bytes))
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(data))
}

#[cfg(test)]
mod checksum_tests {
    use super::sha256_hex;

    #[test]
    fn sha256_standard_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}

// ------------------------------------------------------------------ 下载内核

#[derive(Debug, Clone, Serialize)]
pub struct KernelProgress {
    pub id: String,
    pub downloaded: u64,
    pub total: u64,
    pub percent: f64,
    pub stage: String,
    pub message: String,
}

/// 下载内核并安装到 images/base/kernel
pub async fn install_kernel(
    id: &str,
    progress: impl Fn(KernelProgress) + Clone,
) -> Result<String> {
    let m = fetch_manifest().await?;

    let entry = m
        .kernels
        .iter()
        .find(|k| k.id == id)
        .ok_or_else(|| Error::Other(format!("内核不存在: {id}")))?;

    if entry.url.is_empty() || entry.status != "ready" {
        return Err(Error::Other(format!(
            "内核 {} 尚未编译完成。请先在 GitHub Actions 运行 build-kernel.yml",
            entry.name
        )));
    }

    let c = client()?;
    let dir = kernels_dir();
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(id);

    progress(KernelProgress {
        id: id.into(),
        downloaded: 0,
        total: 0,
        percent: 0.0,
        stage: "downloading".into(),
        message: "开始下载".into(),
    });

    // 依次尝试直连与镜像
    let urls = [
        entry.url.clone(),
        format!("https://ghfast.top/{}", entry.url),
        format!("https://gh-proxy.com/{}", entry.url),
    ];

    let mut ok = false;
    let mut last_err = String::new();
    let mut got = 0u64;
    let mut total = 0u64;

    for u in &urls {
        match c.get(u).send().await {
            Ok(r) if r.status().is_success() => {
                total = r.content_length().unwrap_or(0);
                got = 0;

                let mut f = std::fs::File::create(&dest)?;
                let mut stream = r.bytes_stream();
                use futures_util::StreamExt;

                let mut failed = false;
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(ch) => {
                            f.write_all(&ch)?;
                            got += ch.len() as u64;
                            let pct = if total > 0 {
                                got as f64 / total as f64 * 100.0
                            } else {
                                0.0
                            };
                            progress(KernelProgress {
                                id: id.into(),
                                downloaded: got,
                                total,
                                percent: pct,
                                stage: "downloading".into(),
                                message: format!(
                                    "{:.1} MB",
                                    got as f64 / 1048576.0
                                ),
                            });
                        }
                        Err(e) => {
                            last_err = format!("{e}");
                            failed = true;
                            break;
                        }
                    }
                }
                drop(f);

                if !failed && got > 1024 {
                    ok = true;
                    break;
                }
                let _ = std::fs::remove_file(&dest);
            }
            Ok(r) => last_err = format!("HTTP {}", r.status()),
            Err(e) => last_err = format!("{e}"),
        }
    }

    if !ok {
        return Err(Error::Other(format!("内核下载失败: {last_err}")));
    }

    let expected = entry.sha256.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::Other("内核清单缺少有效SHA256；下载文件已保留，活动内核未改动".into()));
    }
    let actual = crate::managed::sha256_file(&dest)?;
    if actual != expected {
        return Err(Error::Other(format!("内核SHA256不匹配：expected={expected}, actual={actual}；活动内核未改动")));
    }

    // ---- 安装：复制到 images/base/kernel
    progress(KernelProgress {
        id: id.into(),
        downloaded: got,
        total,
        percent: 98.0,
        stage: "installing".into(),
        message: "安装到 base".into(),
    });

    let base = base_images_dir();
    std::fs::create_dir_all(&base)?;

    // 备份旧内核
    let target = base.join("kernel");
    if target.exists() {
        let backup = base.join("kernel.bak");
        let _ = std::fs::copy(&target, &backup);
    }

    std::fs::copy(&dest, &target)?;

    // ---- 记录直接使用标记
    std::fs::write(kernel_marker(id), b"ok")?;

    progress(KernelProgress {
        id: id.into(),
        downloaded: got,
        total,
        percent: 100.0,
        stage: "done".into(),
        message: "安装完成".into(),
    });

    Ok(format!(
        "{} 已安装（{:.1} MB）",
        entry.name,
        got as f64 / 1048576.0
    ))
}

/// 卸载某个内核
pub fn uninstall_kernel(id: &str) -> Result<()> {
    let m = kernel_marker(id);
    if m.exists() {
        let _ = std::fs::remove_file(&m);
    }
    let k = kernels_dir().join(id);
    if k.exists() {
        let _ = std::fs::remove_file(&k);
    }
    Ok(())
}

/// 从备份恢复内核
pub fn restore_backup() -> Result<()> {
    let base = base_images_dir();
    let bak = base.join("kernel.bak");
    if bak.exists() {
        std::fs::copy(&bak, base.join("kernel"))?;
        Ok(())
    } else {
        Err(Error::Other("没有内核备份".into()))
    }
}
