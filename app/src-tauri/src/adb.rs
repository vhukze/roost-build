//! ADB 桥接层
//!
//! 直接调用 adb.exe 并解析输出。所有操作为异步，不阻塞 UI。

use crate::types::*;
use std::process::Stdio;
use tokio::process::Command;

/// 命令执行结果
#[derive(Debug, Clone, serde::Serialize)]
pub struct CmdResult {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl CmdResult {
    pub fn output(&self) -> String {
        if self.stderr.trim().is_empty() {
            self.stdout.trim().to_string()
        } else {
            format!("{}\n{}", self.stdout.trim(), self.stderr.trim())
                .trim()
                .to_string()
        }
    }
}

/// 设备信息
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceInfo {
    pub connected: bool,
    pub serial: String,
    pub model: String,
    pub android_version: String,
    pub sdk: String,
    pub abi: String,
    pub debuggable: String,
    pub root: bool,
}

impl Default for DeviceInfo {
    fn default() -> Self {
        Self {
            connected: false,
            serial: String::new(),
            model: String::new(),
            android_version: String::new(),
            sdk: String::new(),
            abi: String::new(),
            debuggable: String::new(),
            root: false,
        }
    }
}

/// ADB 封装
#[derive(Clone)]
pub struct Adb {
    serial: String,
    executable: std::path::PathBuf,
    su_path: String,
}

impl Adb {
    pub fn new(port: u16) -> Self {
        Self {
            serial: format!("127.0.0.1:{port}"),
            executable: adb_exe(),
            su_path: "/system/bin/su".into(),
        }
    }

    pub fn with_serial(serial: impl Into<String>) -> Self {
        Self {
            serial: serial.into(),
            executable: adb_exe(),
            su_path: "/system/bin/su".into(),
        }
    }

    pub fn for_vm(config: &VmConfig) -> Result<Self> {
        Ok(Self { serial: config.serial()?, executable: if config.backend == "android-emulator" {
            std::path::PathBuf::from(&config.sdk_path).join("platform-tools/adb.exe")
        } else { adb_exe() },
        su_path: if config.backend == "android-emulator" {
            std::path::Path::new(&config.magisk_path).parent().unwrap_or(std::path::Path::new("/debug_ramdisk"))
                .join("su").to_string_lossy().replace('\\', "/")
        } else { "/system/bin/su".into() } })
    }

    /// 执行 adb 命令
    async fn run(&self, args: &[&str]) -> Result<CmdResult> {
        let exe = &self.executable;
        if !exe.exists() {
            return Err(Error::Command("adb 未安装".into()));
        }

        let mut command = Command::new(exe);
        command.args(args).stdin(Stdio::null()).kill_on_drop(true);
        let seconds = if args.iter().any(|v| *v == "install" || *v == "push" || v.contains("--install-module")) { 180 } else { 20 };
        let output = tokio::time::timeout(std::time::Duration::from_secs(seconds), command.output())
            .await.map_err(|_| Error::Other(format!("目标VM ADB命令超时（{seconds}s）；请在Magisk中明确授权Shell后重试")))??;

        Ok(CmdResult {
            ok: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            code: output.status.code().unwrap_or(-1),
        })
    }

    /// 执行带设备序列号的命令
    async fn run_serial(&self, args: &[&str]) -> Result<CmdResult> {
        let mut full = vec!["-s", self.serial.as_str()];
        full.extend_from_slice(args);
        self.run(&full).await
    }

    /// 连接设备
    pub async fn connect(&self) -> bool {
        if self.serial.starts_with("emulator-") {
            return self.run_serial(&["get-state"]).await
                .map(|r| r.ok && r.stdout.trim() == "device").unwrap_or(false);
        }
        let result = self.run(&["connect", &self.serial]).await;
        match result {
            Ok(r) => r.output().to_lowercase().contains("connected"),
            Err(_) => false,
        }
    }

    pub async fn emulator_kill(&self) -> Result<CmdResult> {
        if !self.serial.starts_with("emulator-") { return Err(Error::Other("仅Emulator支持此停止方式".into())); }
        self.run_serial(&["emu", "kill"]).await
    }

    /// 断开连接
    pub async fn disconnect(&self) -> bool {
        if self.serial.starts_with("emulator-") { return false; }
        self.run(&["disconnect", &self.serial])
            .await
            .map(|r| r.ok)
            .unwrap_or(false)
    }

    /// 执行 shell 命令
    pub async fn shell(&self, cmd: &str) -> Result<CmdResult> {
        self.run_serial(&["shell", cmd]).await
    }

    /// Quote one command argument for Android sh; no reliance on guest PATH for su.
    pub async fn su_shell(&self, command: &str) -> Result<CmdResult> {
        let quoted = command.replace('\'', "'\\''");
        self.shell(&format!("{} -c '{}'", self.su_path, quoted)).await
    }

    /// 读取属性
    pub async fn getprop(&self, prop: &str) -> String {
        self.shell(&format!("getprop {prop}"))
            .await
            .map(|r| r.stdout.trim().to_string())
            .unwrap_or_default()
    }

    /// 系统是否启动完成
    pub async fn is_booted(&self) -> bool {
        self.getprop("sys.boot_completed").await.trim() == "1"
    }

    /// 等待启动完成
    pub async fn wait_for_boot(&self, timeout_secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        while std::time::Instant::now() < deadline {
            if self.is_booted().await {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        false
    }

    /// 检测 root
    pub async fn check_root(&self) -> bool {
        self.su_shell("id")
            .await
            .map(|r| r.ok && r.stdout.contains("uid=0"))
            .unwrap_or(false)
    }

    /// 采集设备信息
    pub async fn device_info(&self) -> DeviceInfo {
        let mut info = DeviceInfo {
            serial: self.serial.clone(),
            connected: true,
            ..Default::default()
        };

        info.model = self.getprop("ro.product.model").await;
        info.android_version = self.getprop("ro.build.version.release").await;
        info.sdk = self.getprop("ro.build.version.sdk").await;
        info.abi = self.getprop("ro.product.cpu.abi").await;
        info.debuggable = self.getprop("ro.debuggable").await;
        info.root = self.check_root().await;

        if info.model.is_empty() {
            info.connected = false;
        }

        info
    }

    /// 安装 APK
    pub async fn install(&self, apk: &str) -> Result<CmdResult> {
        self.run_serial(&["install", "-r", apk]).await
    }

    /// 卸载应用
    pub async fn uninstall(&self, package: &str) -> Result<CmdResult> {
        self.run_serial(&["uninstall", package]).await
    }

    /// 列出第三方应用
    pub async fn list_packages(&self) -> Vec<String> {
        self.shell("pm list packages -3")
            .await
            .map(|r| {
                r.stdout
                    .lines()
                    .filter_map(|l| l.strip_prefix("package:"))
                    .map(|s| s.trim().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 抓取 logcat
    pub async fn logcat(&self, lines: u32, buffer: &str) -> String {
        self.shell(&format!("logcat -d -t {lines} -b {buffer}"))
            .await
            .map(|r| r.stdout)
            .unwrap_or_default()
    }

    /// 抓取崩溃日志
    pub async fn crash_log(&self, lines: u32) -> String {
        self.logcat(lines, "crash").await
    }

    /// 强制停止应用
    pub async fn force_stop(&self, package: &str) -> Result<CmdResult> {
        self.shell(&format!("am force-stop {package}")).await
    }

    /// 启动应用
    pub async fn start_app(&self, package: &str) -> Result<CmdResult> {
        self.shell(&format!(
            "monkey -p {package} -c android.intent.category.LAUNCHER 1"
        ))
        .await
    }

    /// 进程是否存活
    pub async fn is_alive(&self, package: &str) -> bool {
        self.shell(&format!("pidof {package}"))
            .await
            .map(|r| !r.stdout.trim().is_empty())
            .unwrap_or(false)
    }

    /// 列出 tombstone
    pub async fn tombstones(&self) -> Vec<String> {
        self.shell("ls /data/tombstones/ 2>/dev/null")
            .await
            .map(|r| {
                r.stdout
                    .lines()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 读取 tombstone 内容
    pub async fn read_tombstone(&self, name: &str) -> String {
        self.shell(&format!("cat /data/tombstones/{name} 2>/dev/null"))
            .await
            .map(|r| r.stdout)
            .unwrap_or_default()
    }

    /// 文件推送
    pub async fn push(&self, local: &str, remote: &str) -> Result<CmdResult> {
        self.run_serial(&["push", local, remote]).await
    }
}
