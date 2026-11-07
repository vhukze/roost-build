//! 版本搜索与精选列表

use crate::types::*;

/// 每个管理器的精选旧版本（代表性版本）
///
/// 只保留有特殊意义的版本：
///   - Magisk 24.0 之前有 MagiskHide，之后移除
///   - Magisk 26.0 引入新版 Zygisk
///   - KernelSU 0.9.x 是非 GKI 支持最完整的
fn featured_versions(manager: &str) -> Vec<&'static str> {
    match manager {
        "magisk" => vec!["25.2", "24.3", "23.0", "20.4"],
        "ksu" => vec!["0.9.5", "0.7.2", "0.6.9"],
        "apatch" => vec!["10657"],
        "sukisu" => vec!["3.8.2"],
        "ksu-next" => vec!["1.0.9"],
        _ => vec![],
    }
}

/// 判断是否精选版本
pub fn is_featured(manager: &str, version: &str) -> bool {
    featured_versions(manager).iter().any(|v| *v == version)
}

/// 搜索版本：在给定列表中按版本号模糊匹配
pub fn search_versions<'a>(
    versions: &'a [crate::roots::RootVersion],
    query: &str,
) -> Vec<&'a crate::roots::RootVersion> {
    let q = query.trim().trim_start_matches('v').to_lowercase();
    if q.is_empty() {
        return versions.iter().collect();
    }
    versions
        .iter()
        .filter(|v| v.version.to_lowercase().contains(&q))
        .collect()
}

// ------------------------------------------------------------------ LSPosed 版本

/// LSPosed 精选版本
pub fn lsposed_featured() -> Vec<&'static str> {
    vec!["1.9.2", "1.9.1", "1.9.0", "1.8.6", "1.8.5"]
}

/// LSPosed 变体说明
pub fn lsposed_variants() -> Vec<(&'static str, &'static str)> {
    vec![
        ("zygisk", "Zygisk 版（推荐，需 Magisk/KernelSU 支持）"),
        ("riru", "Riru 版（旧方案，兼容性更好）"),
    ]
}

// ------------------------------------------------------------------ 模块搜索

/// 模块仓库的组织名
pub const MODULE_ORG: &str = "Xposed-Modules-Repo";

/// 从仓库名提取包名（去掉组织前缀）
pub fn repo_to_package(full_name: &str) -> String {
    full_name
        .strip_prefix(&format!("{MODULE_ORG}/"))
        .unwrap_or(full_name)
        .to_string()
}

/// 构造模块仓库搜索 URL
pub fn module_search_url(query: &str, page: u32) -> String {
    let q = if query.trim().is_empty() {
        "topic:lsposed-module".to_string()
    } else {
        format!("{}", query.trim())
    };
    format!(
        "https://api.github.com/search/repositories?q={}&sort=stars&order=desc&per_page=30&page={page}",
        urlencode(&q)
    )
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 模块信息
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModuleInfo {
    pub full_name: String,
    pub package: String,
    pub name: String,
    pub description: String,
    pub stars: u32,
    pub updated: String,
    pub html_url: String,
    /// 最新 release 的 APK 下载地址（懒加载）
    #[serde(default)]
    pub apk_url: String,
    #[serde(default)]
    pub apk_name: String,
    #[serde(default)]
    pub version: String,
}

/// 从 GitHub 搜索 API 响应解析模块列表
pub fn parse_module_search(body: &str) -> Vec<ModuleInfo> {
    #[derive(serde::Deserialize)]
    struct Resp {
        #[serde(default)]
        items: Vec<Item>,
    }
    #[derive(serde::Deserialize)]
    struct Item {
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

    let resp: Resp = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    resp.items
        .into_iter()
        .map(|i| {
            let pkg = repo_to_package(&i.full_name);
            ModuleInfo {
                package: pkg.clone(),
                name: pkg,
                description: i.description.unwrap_or_default(),
                stars: i.stargazers_count,
                updated: i.updated_at.chars().take(10).collect(),
                html_url: i.html_url,
                full_name: i.full_name,
                apk_url: String::new(),
                apk_name: String::new(),
                version: String::new(),
            }
        })
        .collect()
}
