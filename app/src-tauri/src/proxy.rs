//! 代理探测
//!
//! 优先级：
//!   1. 环境变量 ROOST_PROXY
//!   2. 环境变量 HTTPS_PROXY / HTTP_PROXY
//!   3. Windows 系统代理（注册表 Internet Settings）

/// 探测可用代理，返回形如 "http://127.0.0.1:7897"
pub fn detect() -> Option<String> {
    // 1. 专用环境变量
    if let Ok(v) = std::env::var("ROOST_PROXY") {
        let v = v.trim();
        if !v.is_empty() {
            return Some(normalize(v));
        }
    }

    // 2. 标准环境变量
    for key in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(normalize(v));
            }
        }
    }

    // 3. Windows 系统代理
    #[cfg(windows)]
    {
        if let Some(p) = windows_system_proxy() {
            return Some(p);
        }
    }

    None
}

fn normalize(raw: &str) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") || raw.starts_with("socks") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    }
}

#[cfg(windows)]
fn windows_system_proxy() -> Option<String> {
    use std::process::Command;

    let out = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyServer",
        ])
        .output()
        .ok()?;

    let text = String::from_utf8_lossy(&out.stdout);

    for line in text.lines() {
        if !line.contains("ProxyServer") {
            continue;
        }
        let value = line.split_whitespace().last()?;
        // 可能形如 "127.0.0.1:7897" 或 "http=127.0.0.1:7897;https=..."
        let candidate = if value.contains('=') {
            value
                .split(';')
                .find_map(|seg| seg.split_once('=').map(|(_, v)| v))
                .unwrap_or(value)
        } else {
            value
        };
        if candidate.contains(':') {
            return Some(normalize(candidate));
        }
    }
    None
}
