//! 崩溃诊断与归因
//!
//! 针对「ARM 应用在模拟器里闪退」这个核心痛点，
//! 自动判断崩溃阶段、归因类别，并给出排查建议。

use crate::adb::Adb;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

// ------------------------------------------------------------------ 枚举

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrashPhase {
    Launch,
    UiShown,
    Interact,
    Login,
    Unknown,
}

impl CrashPhase {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Launch => "启动即崩溃",
            Self::UiShown => "UI 显示后崩溃",
            Self::Interact => "交互过程中崩溃",
            Self::Login => "登录 / 联网后崩溃",
            Self::Unknown => "无法判断",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrashCause {
    Translation,
    Graphics,
    NativeAbi,
    SystemApi,
    Detection,
    Permission,
    Oom,
    Unknown,
}

impl CrashCause {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Translation => "指令翻译（TCG 语义）",
            Self::Graphics => "图形层（GLES / Vulkan）",
            Self::NativeAbi => "原生库 ABI 不匹配",
            Self::SystemApi => "系统 API / HAL 缺失",
            Self::Detection => "环境检测（root / 模拟器）",
            Self::Permission => "权限 / SELinux",
            Self::Oom => "内存不足",
            Self::Unknown => "未归因",
        }
    }

    pub fn suggestions(&self) -> Vec<&'static str> {
        match self {
            Self::Translation => vec![
                "增大 TCG 翻译缓存: tb_size_mb 调到 2048",
                "尝试切换 CPU 型号: cortex-a72 → max",
                "确认 QEMU 版本 >= 8.0（旧版 NEON 语义有已知 bug）",
                "抓取 SIGILL 的完整 backtrace，定位具体指令地址",
            ],
            Self::NativeAbi => vec![
                "检查 APK 是否只含 armeabi-v7a（需要 32 位支持）",
                "解包 APK 确认 lib/ 目录下的 ABI",
                "若只有 arm64-v8a，确认镜像为 arm64 版本",
                "检查 dlopen 失败的具体 .so 名",
            ],
            Self::Graphics => vec![
                "确认已加载 virtio-gpu-pci 设备",
                "尝试强制软件渲染: setprop debug.hwui.renderer skiavk",
                "检查是否使用了 Vulkan（SwiftShader 支持有限）",
                "降级到 GLES 2.0: setprop debug.egl.glEsVersion 2",
            ],
            Self::Detection => vec![
                "抓取触发检测的具体路径（su / magisk / ro.debuggable）",
                "在 Root overlay 的 clean 清单中加入对应路径清理",
                "考虑切换到 none overlay 验证是否检测导致",
                "LSPosed 可 hook 检测函数，但优先级低于先解决翻译问题",
            ],
            Self::SystemApi => vec![
                "确认镜像未裁剪该 API 对应的服务",
                "检查 logcat 中 ServiceNotFoundException 的具体服务名",
                "尝试完整 AOSP 镜像（非精简版）",
            ],
            Self::Permission => vec![
                "检查 avc: denied 的具体规则",
                "在 overlay sepolicy 中添加对应 allow 规则",
                "临时验证: setenforce 0（仅调试用）",
            ],
            Self::Oom => vec![
                "增大虚拟机内存: memory_mb 调到 8192",
                "检查 lowmemorykiller 的 minfree 配置",
            ],
            Self::Unknown => vec![
                "抓完整 logcat（不止 crash buffer）",
                "用 headless 模式观察 QEMU 串口输出",
                "确认应用是否在真机上正常（排除应用本身问题）",
            ],
        }
    }
}

// ------------------------------------------------------------------ 报告

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub package: String,
    pub timestamp: String,
    pub phase: String,
    pub phase_label: String,
    pub cause: String,
    pub cause_label: String,
    pub confidence: f64,
    pub evidence: Vec<String>,
    pub suggestions: Vec<String>,
    pub matched_signals: std::collections::HashMap<String, Vec<String>>,
    pub crash_log: String,
    pub tombstone: String,
}

// ------------------------------------------------------------------ 规则

struct Rule {
    cause: CrashCause,
    patterns: Vec<&'static str>,
    weight: f64,
}

fn rules() -> &'static Vec<Rule> {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            Rule {
                cause: CrashCause::Translation,
                patterns: vec![
                    r"SIGILL",
                    r"Illegal instruction",
                    r"signal 4 \(SIGILL\)",
                    r"unimplemented instruction",
                    r"invalid instruction",
                ],
                weight: 0.90,
            },
            Rule {
                cause: CrashCause::Translation,
                patterns: vec![
                    r"qemu: uncaught target signal",
                    r"TCG",
                    r"tb_exec",
                    r"helper_",
                    r"unhandled CPU instruction",
                ],
                weight: 0.85,
            },
            Rule {
                cause: CrashCause::NativeAbi,
                patterns: vec![
                    r"UnsatisfiedLinkError",
                    r"library .* not found",
                    r"dlopen failed",
                    r"cannot locate symbol",
                    r"wrong ELF class",
                    r"has text relocations",
                    r"exec format error",
                ],
                weight: 0.85,
            },
            Rule {
                cause: CrashCause::Graphics,
                patterns: vec![
                    r"EGL_BAD_",
                    r"libEGL",
                    r"libGLES",
                    r"gralloc",
                    r"SurfaceFlinger",
                    r"VK_ERROR",
                    r"swiftshader",
                    r"virtio_gpu",
                ],
                weight: 0.80,
            },
            Rule {
                cause: CrashCause::Detection,
                patterns: vec![
                    r"/system/bin/su",
                    r"/system/xbin/su",
                    r"magisk",
                    r"kernelsu",
                    r"zygisk",
                    r"isEmulator",
                    r"isRooted",
                    r"SafetyNet",
                    r"PlayIntegrity",
                    r"ro\.debuggable",
                    r"检测到",
                    r"模拟器",
                ],
                weight: 0.75,
            },
            Rule {
                cause: CrashCause::SystemApi,
                patterns: vec![
                    r"ClassNotFoundException",
                    r"NoSuchMethodError",
                    r"ServiceNotFoundException",
                    r"DeadObjectException",
                    r"HAL.*not found",
                    r"CameraService",
                    r"SensorService",
                ],
                weight: 0.70,
            },
            Rule {
                cause: CrashCause::Permission,
                patterns: vec![
                    r"Permission denied",
                    r"avc:\s+denied",
                    r"SELinux",
                    r"EACCES",
                    r"EPERM",
                ],
                weight: 0.70,
            },
            Rule {
                cause: CrashCause::Oom,
                patterns: vec![
                    r"OutOfMemoryError",
                    r"lowmemorykiller",
                    r"killed by low memory",
                ],
                weight: 0.65,
            },
        ]
    })
}

/// 在文本中匹配规则，返回匹配到的行
fn match_patterns(text: &str, patterns: &[&str]) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut matched = Vec::new();
    for line in text.lines() {
        for pat in patterns {
            if let Ok(re) = Regex::new(&format!("(?i){pat}")) {
                if re.is_match(line) {
                    let trimmed = line.trim();
                    if trimmed.len() > 200 {
                        matched.push(trimmed[..200].to_string());
                    } else {
                        matched.push(trimmed.to_string());
                    }
                    break;
                }
            }
        }
    }
    matched
}

/// 判断崩溃阶段
fn detect_phase(crash_log: &str, logcat: &str) -> CrashPhase {
    let combined = format!("{crash_log}\n{logcat}");

    let has_ui = [
        r"ActivityTaskManager.*Displayed",
        r"ViewRootImpl",
        r"Choreographer",
    ]
    .iter()
    .any(|p| Regex::new(p).map(|re| re.is_match(&combined)).unwrap_or(false));

    let has_network = [
        r"NetworkSecurityConfig",
        r"OkHttp",
        r"HttpClient",
        r"SSLHandshake",
    ]
    .iter()
    .any(|p| Regex::new(p).map(|re| re.is_match(&combined)).unwrap_or(false));

    if has_network {
        CrashPhase::Login
    } else if has_ui {
        CrashPhase::UiShown
    } else {
        CrashPhase::Launch
    }
}

/// 归因分析
fn classify(
    crash_log: &str,
    logcat: &str,
    tombstone: &str,
) -> (CrashCause, f64, std::collections::HashMap<String, Vec<String>>, Vec<String>) {
    let combined = format!("{crash_log}\n{logcat}\n{tombstone}");

    let mut best_cause = CrashCause::Unknown;
    let mut best_score = 0.0f64;
    let mut evidence = Vec::new();
    let mut signals: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for rule in rules() {
        let matched = match_patterns(&combined, &rule.patterns);
        if matched.is_empty() {
            continue;
        }

        let key = rule.cause.label().to_string();
        signals
            .entry(key)
            .or_default()
            .extend(matched.iter().take(5).cloned());

        // 多个信号叠加提高置信度
        let score = rule.weight * (0.5 + 0.15 * matched.len() as f64).min(1.0);
        if score > best_score {
            best_score = score;
            best_cause = rule.cause;
            evidence = matched.into_iter().take(5).collect();
        }
    }

    (best_cause, best_score, signals, evidence)
}

/// 执行完整诊断
pub async fn diagnose(adb: &Adb, package: &str, wait_secs: u64) -> Report {
    let now = chrono_lite_now();

    // 清空日志
    let _ = adb.shell("logcat -c").await;

    // 启动应用
    let _ = adb.force_stop(package).await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let _ = adb.start_app(package).await;

    // 观察
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
    let mut crashed = false;
    while std::time::Instant::now() < deadline {
        if !adb.is_alive(package).await {
            crashed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    // 抓日志
    let crash_log = adb.crash_log(200).await;
    let logcat = adb.logcat(1000, "main").await;

    // 抓 tombstone
    let tombstones = adb.tombstones().await;
    let tombstone = if let Some(latest) = tombstones.last() {
        adb.read_tombstone(latest).await
    } else {
        String::new()
    };

    let phase = detect_phase(&crash_log, &logcat);
    let (cause, confidence, signals, mut evidence) =
        classify(&crash_log, &logcat, &tombstone);

    if !tombstones.is_empty() {
        evidence.insert(0, format!("tombstone: {}", tombstones.last().unwrap()));
    }
    if !crashed {
        evidence.push("观察期内进程未退出（可能未崩溃）".into());
    }

    Report {
        package: package.to_string(),
        timestamp: now,
        phase: format!("{phase:?}"),
        phase_label: phase.label().to_string(),
        cause: format!("{cause:?}"),
        cause_label: cause.label().to_string(),
        confidence,
        evidence,
        suggestions: cause.suggestions().iter().map(|s| s.to_string()).collect(),
        matched_signals: signals,
        crash_log,
        tombstone,
    }
}

/// 极简时间戳（避免引入 chrono 依赖）
fn chrono_lite_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // 简单的 UTC 时间换算
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;

    format!("day{days} {h:02}:{m:02}:{s:02}")
}
