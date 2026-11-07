//! AOSP 系统镜像管理
//!
//! 从 Google 官方仓库下载预编译 arm64 镜像，解压为 QEMU 可用格式。
//! 无需本地编译 AOSP。
//!
//! 镜像清单来源：
//!   https://dl.google.com/android/repository/sys-img/google_apis/sys-img2-4.xml

use crate::types::*;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

// ------------------------------------------------------------------ 数据

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageEntry {
    pub api: u32,
    pub tag: String,
    pub abi: String,
    /// 包大小，如 "1.5 GB"
    pub size: String,
    pub url: String,
    /// 是否已下载到本地
    pub downloaded: bool,
    /// 支持的 Android 版本
    pub android_version: String,
    /// 额外说明
    pub note: String,
}

const SYSIMG_XML: &str =
    "https://dl.google.com/android/repository/sys-img/google_apis/sys-img2-4.xml";

pub fn base_images_dir() -> PathBuf {
    images_dir().join("base")
}

fn image_marker(api: u32, abi: &str) -> PathBuf {
    images_dir()
        .join("aosp-cache")
        .join(format!("api{api}-{abi}.downloaded"))
}

fn aosp_cache_dir() -> PathBuf {
    images_dir().join("aosp-cache")
}

// ------------------------------------------------------------------ 网络

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("Roost/0.1")
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .map_err(|e| Error::Other(format!("{e}")))
}

// ------------------------------------------------------------------ 查询

/// 查询可用的 arm64 系统镜像
pub async fn list_images() -> Result<Vec<ImageEntry>> {
    let c = client()?;

    let text = c
        .get(SYSIMG_XML)
        .send()
        .await
        .map_err(|e| Error::Other(format!("获取镜像清单失败: {e}")))?
        .text()
        .await
        .map_err(|e| Error::Other(format!("{e}")))?;

    let mut out = Vec::new();

    // 逐块解析 <remotePackage path="system-images;android-N;TAG;ABI">
    let mut rest = text.as_str();
    while let Some(idx) = rest.find("<remotePackage path=\"system-images;") {
        let tail = &rest[idx..];
        let end = match tail.find("</remotePackage>") {
            Some(e) => e + "</remotePackage>".len(),
            None => break,
        };
        let block = &tail[..end];
        rest = &tail[end..];

        // 路径三段
        let path_m = match extract(block, "path=\"", "\"") {
            Some(p) => p,
            None => continue,
        };
        let parts: Vec<&str> = path_m.split(';').collect();
        if parts.len() < 4 {
            continue;
        }
        let api: u32 = match parts[1].trim_start_matches("android-").parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tag = parts[2].to_string();
        let abi = parts[3].to_string();

        if abi != "arm64-v8a" {
            continue;
        }

        // 只要 google_apis（不要 playstore / ps16k 变体，避免 root 限制）
        if tag != "google_apis" {
            continue;
        }

        let size_bytes: u64 = extract(block, "<size>", "</size>")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let url = match extract(block, "<url>", "</url>") {
            Some(u) => u,
            None => continue,
        };

        let full_url = format!(
            "https://dl.google.com/android/repository/sys-img/google_apis/{url}"
        );

        out.push(ImageEntry {
            api,
            tag,
            abi: abi.clone(),
            size: format!("{:.2} GB", size_bytes as f64 / 1024.0 / 1024.0 / 1024.0),
            url: full_url,
            downloaded: image_marker(api, &abi).exists(),
            android_version: android_version_name(api),
            note: image_note(api),
        });
    }

    out.sort_by(|a, b| b.api.cmp(&a.api));
    Ok(out)
}

fn extract(s: &str, start: &str, end: &str) -> Option<String> {
    let i = s.find(start)? + start.len();
    let rest = &s[i..];
    let j = rest.find(end)?;
    Some(rest[..j].trim().to_string())
}

fn android_version_name(api: u32) -> String {
    match api {
        36 => "Android 16",
        35 => "Android 15",
        34 => "Android 14",
        33 => "Android 13",
        32 => "Android 12L",
        31 => "Android 12",
        30 => "Android 11",
        29 => "Android 10",
        28 => "Android 9",
        27 => "Android 8.1",
        _ => "Android ?",
    }
    .into()
}

fn image_note(api: u32) -> String {
    if api >= 34 {
        "新版，16KB 页大小支持".into()
    } else if api >= 30 {
        "推荐，兼容性平衡".into()
    } else {
        "旧版，镜像小、启动快".into()
    }
}

// ------------------------------------------------------------------ 下载

#[derive(Debug, Clone, Serialize)]
pub struct ImageProgress {
    pub api: u32,
    pub downloaded: u64,
    pub total: u64,
    pub percent: f64,
    pub stage: String,
    pub message: String,
}

/// 下载并解压系统镜像到 images/base/
pub async fn download_image(
    api: u32,
    url: &str,
    progress: impl Fn(ImageProgress) + Clone,
) -> Result<String> {
    let c = client()?;
    let dir = aosp_cache_dir();
    std::fs::create_dir_all(&dir)?;

    let zip_path = dir.join(format!("arm64-api{api}.zip"));

    // ---- 下载
    progress(ImageProgress {
        api,
        downloaded: 0,
        total: 0,
        percent: 0.0,
        stage: "downloading".into(),
        message: "开始下载".into(),
    });

    let resp = c
        .get(url)
        .send()
        .await
        .map_err(|e| Error::Other(format!("下载失败: {e}")))?;

    if !resp.status().is_success() {
        return Err(Error::Other(format!("HTTP {}", resp.status())));
    }

    let total = resp.content_length().unwrap_or(0);
    let mut got = 0u64;

    let mut f = std::fs::File::create(&zip_path)?;
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Error::Other(format!("{e}")))?;
        f.write_all(&chunk)?;
        got += chunk.len() as u64;

        let pct = if total > 0 { got as f64 / total as f64 * 100.0 } else { 0.0 };
        progress(ImageProgress {
            api,
            downloaded: got,
            total,
            percent: pct * 0.9, // 下载占 90%
            stage: "downloading".into(),
            message: format!("{:.0} MB / {:.0} MB", got as f64 / 1048576.0, total as f64 / 1048576.0),
        });
    }
    drop(f);

    // ---- 解压
    progress(ImageProgress {
        api,
        downloaded: got,
        total,
        percent: 90.0,
        stage: "extracting".into(),
        message: "解压中".into(),
    });

    let base = base_images_dir();
    if base.exists() {
        // 清空旧的 base（保留非镜像文件）
        for e in std::fs::read_dir(&base)?.flatten() {
            let p = e.path();
            if p.is_file() {
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                if name.ends_with(".img")
                    || name == "kernel"
                    || name.starts_with("kernel-")
                    || name == "ramdisk.img"
                {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }
    std::fs::create_dir_all(&base)?;

    let zf = std::fs::File::open(&zip_path)?;
    let mut zip = zip::ZipArchive::new(zf)
        .map_err(|e| Error::Other(format!("zip 解析失败: {e}")))?;

    let count = zip.len();
    let mut extracted = 0;

    for i in 0..count {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| Error::Other(format!("读取条目失败: {e}")))?;

        if entry.is_dir() {
            continue;
        }

        let raw_name = entry.name().to_string();
        let base_name = std::path::Path::new(&raw_name)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        // 只保留需要的文件
        let keep = base_name == "kernel"
            || base_name.starts_with("kernel-")
            || base_name == "ramdisk.img"
            || base_name == "system.img"
            || base_name == "vendor.img"
            || base_name == "userdata.img"
            || base_name == "build.prop";

        if !keep {
            continue;
        }

        let dest = base.join(&base_name);
        let mut out = std::fs::File::create(&dest)?;
        std::io::copy(&mut entry, &mut out)?;
        extracted += 1;

        let pct = 90.0 + (extracted as f64 / 6.0).min(1.0) * 8.0;
        progress(ImageProgress {
            api,
            downloaded: got,
            total,
            percent: pct,
            stage: "extracting".into(),
            message: format!("解压 {base_name}"),
        });
    }

    if extracted == 0 {
        return Err(Error::Other("包内未找到镜像文件".into()));
    }

    // ---- 转换为 qcow2
    progress(ImageProgress {
        api,
        downloaded: got,
        total,
        percent: 98.0,
        stage: "converting".into(),
        message: "转换格式".into(),
    });

    for part in ["system", "vendor"] {
        let src = base.join(format!("{part}.img"));
        let dst = base.join(format!("{part}.qcow2"));
        if !src.exists() {
            continue;
        }
        let _ = crate::background::async_command(qemu_img_exe())
            .args([
                "convert",
                "-f",
                "raw",
                "-O",
                "qcow2",
                src.to_string_lossy().as_ref(),
                dst.to_string_lossy().as_ref(),
            ])
            .output()
            .await;
        // 转换成功后删除原始 img 省空间
        if dst.exists() {
            let _ = std::fs::remove_file(&src);
        }
    }

    // ---- 标记完成
    std::fs::create_dir_all(aosp_cache_dir())?;
    std::fs::write(image_marker(api, "arm64-v8a"), b"ok")?;
    let _ = std::fs::remove_file(&zip_path);

    progress(ImageProgress {
        api,
        downloaded: got,
        total,
        percent: 100.0,
        stage: "done".into(),
        message: format!("完成，提取 {extracted} 个文件"),
    });

    Ok(format!("API {api} 镜像已就绪，提取 {extracted} 个文件"))
}

/// 删除已安装的镜像
pub fn remove_image(api: u32) -> Result<()> {
    let base = base_images_dir();
    if base.exists() {
        for e in std::fs::read_dir(&base)?.flatten() {
            let p = e.path();
            if p.is_file() {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    let m = image_marker(api, "arm64-v8a");
    if m.exists() {
        let _ = std::fs::remove_file(&m);
    }
    Ok(())
}
