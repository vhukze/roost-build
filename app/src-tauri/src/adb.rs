//! ADB 桥接层
//!
//! 直接调用 adb.exe 并解析输出。所有操作为异步，不阻塞 UI。

use crate::types::*;
use std::process::Stdio;


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

/// Nonroot boot observation; false is a native observation, not a string fallback.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VmBootStatus {
    pub boot_completed: bool,
    pub serial: String,
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
    /// Observational device_info never prompts for root; false means untested.
    pub root_checked: bool,
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
            root_checked: false,
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

    /// 执行 adb 命令；existing general 20s / install 180s defaults stay unchanged.
    async fn run(&self, args: &[&str]) -> Result<CmdResult> {
        let seconds = if args.iter().any(|v| *v == "install" || *v == "push" || v.contains("--install-module")) { 180 } else { 20 };
        self.run_with_timeout(args, seconds).await
    }

    async fn run_with_timeout(&self, args: &[&str], seconds: u64) -> Result<CmdResult> {
        let exe = &self.executable;
        if !exe.exists() {
            return Err(Error::Command("adb 未安装".into()));
        }

        let mut command = crate::background::async_command(exe);
        command.args(args).stdin(Stdio::null()).kill_on_drop(true);
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

    /// Capture only this serial's guest framebuffer, preserving binary PNG bytes.
    /// There is deliberately no caller-supplied command or output path.
    pub async fn capture_png(&self) -> Result<Vec<u8>> {
        use tokio::io::AsyncReadExt;

        const MAX_PNG_BYTES: u64 = 64 * 1024 * 1024;
        if !self.executable.is_file() {
            return Err(Error::Command("adb 未安装".into()));
        }
        let mut command = crate::background::async_command(&self.executable);
        command.args(["-s", self.serial.as_str(), "exec-out", "screencap", "-p"])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| Error::Other("ADB PNG stdout未接管".into()))?;
        let stderr = child.stderr.take().ok_or_else(|| Error::Other("ADB PNG stderr未接管".into()))?;
        let operation = async {
            let mut png = Vec::new();
            let mut errors = Vec::new();
            tokio::try_join!(
                async { stdout.take(MAX_PNG_BYTES + 1).read_to_end(&mut png).await },
                async { stderr.take(8193).read_to_end(&mut errors).await }
            )?;
            if png.len() as u64 > MAX_PNG_BYTES {
                return Err(Error::Other("Guest PNG超过64MiB上限".into()));
            }
            let status = child.wait().await?;
            if !status.success() {
                return Err(Error::Command(format!("Guest screencap失败: {}", String::from_utf8_lossy(&errors))));
            }
            validate_guest_png(&png)?;
            Ok(png)
        };
        let result = match tokio::time::timeout(std::time::Duration::from_secs(20), operation).await {
            Ok(result) => result,
            Err(_) => Err(Error::Other("Guest screencap超时（20s）".into())),
        };
        if result.is_err() {
            // This handle identifies only the helper spawned above, never an emulator.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), child.kill()).await;
        }
        result
    }

    pub fn serial(&self) -> &str {
        &self.serial
    }

    pub(crate) async fn guest_boot_id(&self) -> Result<CmdResult> {
        self.run_serial(&["shell", "cat", "/proc/sys/kernel/random/boot_id"]).await
    }

    pub(crate) async fn guest_boot_completed(&self) -> Result<CmdResult> {
        self.run_serial(&["shell", "getprop", "sys.boot_completed"]).await
    }

    /// Called only after guest::tap validates the expected boot, PNG size and bounds.
    /// Decimal u32 coordinates are the only variable tokens; no shell text or key input.
    pub(crate) async fn tap_guest(&self, x: u32, y: u32) -> Result<CmdResult> {
        let x = x.to_string();
        let y = y.to_string();
        self.run_serial(&["shell", "input", "tap", &x, &y]).await
    }

    /// Launch only an installed package's MAIN/LAUNCHER activity on this fixed serial.
    /// Every caller-controlled token is validated before any ADB command is spawned.
    pub async fn launch_launcher(&self, package_name: &str) -> Result<CmdResult> {
        validate_launch_package(package_name)?;
        let installed = self.run_serial(&["shell", "pm", "path", package_name]).await?;
        if !installed.ok || !installed.stdout.lines().any(|line| {
            line.trim().strip_prefix("package:").map(|path| path.starts_with('/') && path.ends_with(".apk")).unwrap_or(false)
        }) {
            return Err(Error::Command(format!("目标VM未确认已安装包 {package_name}: {}", installed.output())));
        }
        let resolved = self.run_serial(&[
            "shell", "cmd", "package", "resolve-activity", "--brief",
            "-a", "android.intent.action.MAIN", "-c", "android.intent.category.LAUNCHER",
            "-p", package_name,
        ]).await?;
        if !resolved.ok {
            return Err(Error::Command(format!("解析Launcher失败: {}", resolved.output())));
        }
        let component = parse_launcher_component(package_name, &resolved.stdout).map_err(|error| {
            Error::Command(format!("{error}; resolve-activity: {}", resolved.output()))
        })?;
        // Android's shell interprets '$' in nested Java class names. Single quoting is safe
        // because the component parser permits only ASCII identifier tokens, dots and '/'.
        let quoted_component = format!("'{component}'");
        let result = self.run_serial(&[
            "shell", "am", "start", "-W", "-a", "android.intent.action.MAIN",
            "-c", "android.intent.category.LAUNCHER", "-n", &quoted_component,
        ]).await?;
        if !launcher_start_succeeded(&result) {
            return Err(Error::Command(format!("启动Launcher失败: {}", result.output())));
        }
        Ok(result)
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
        // `emu kill` cuts power to the guest. Flush the guest page cache first, otherwise
        // writes made just before stopping (module install, Magisk state, app data) can be
        // lost on the next cold boot. A failed sync must not skip the stop itself.
        let _ = self.run_serial(&["shell", "sync"]).await;
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

    /// Observe only this owned serial; no su, connection takeover or root prompt.
    pub async fn boot_status(&self) -> Result<VmBootStatus> {
        let state = self.run_serial(&["get-state"]).await?;
        if !state.ok || state.stdout.trim() != "device" {
            return Ok(VmBootStatus { boot_completed: false, serial: self.serial.clone() });
        }
        let completed = self.guest_boot_completed().await?;
        if !completed.ok {
            return Err(Error::Command(format!("读取Guest启动状态失败（exit={}）: {}", completed.code, completed.output())));
        }
        Ok(VmBootStatus { boot_completed: completed.stdout.trim() == "1", serial: self.serial.clone() })
    }

    /// Explicit user request only: one fixed id command, waiting at most 90 seconds.
    /// su_path derives exclusively from a validated owned-running VM configuration.
    pub async fn su_id_request(&self) -> Result<CmdResult> {
        if !self.boot_status().await.map_err(|error| Error::Command(error.to_string()))?.boot_completed {
            return Err(Error::Command("目标VM尚未完成启动；未发送Root请求".into()));
        }
        let result = self.run_with_timeout(
            &["-s", self.serial.as_str(), "shell", self.su_path.as_str(), "-c", "'id'"], 90,
        ).await.map_err(|error| Error::Command(format!("显式Root请求失败: {error}")))?;
        if !root_id_succeeded(&result) {
            return Err(Error::Command(format!("显式Root请求未确认UID 0（exit={}）: {}", result.code, result.output())));
        }
        Ok(result)
    }

    /// Existing explicit verifiers retain their general timeout, with exact UID parsing.
    pub async fn check_root(&self) -> bool {
        self.su_shell("id").await.map(|r| root_id_succeeded(&r)).unwrap_or(false)
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
        // Root is unknown here. Only an explicit request/verifier may invoke su.
        info.root = false;
        info.root_checked = false;

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

/// Accept the real id UID field only, never uid=00, euid=0 or incidental text.
fn root_id_succeeded(result: &CmdResult) -> bool {
    if !result.ok || result.code != 0 || result.stdout.trim().lines().count() != 1 { return false; }
    let Some(uid) = result.stdout.split_whitespace().next().and_then(|field| field.strip_prefix("uid=")) else { return false; };
    if uid == "0" { return true; }
    match uid.split_once('(') {
        Some(("0", name)) => name.strip_suffix(')').map(|name| !name.is_empty()
            && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))).unwrap_or(false),
        _ => false,
    }
}

#[cfg(test)]
mod root_request_tests {
    use super::*;

    #[test]
    fn root_request_requires_exit_zero_and_exact_id_uid_field() {
        let result = |ok, code, stdout: &str| CmdResult { ok, code, stdout: stdout.into(), stderr: String::new() };
        for stdout in ["uid=0(root) gid=0(root) groups=0(root) context=u:r:magisk:s0\n", "uid=0 gid=0", "uid=0(root)\r\n"] {
            assert!(root_id_succeeded(&result(true, 0, stdout)), "{stdout}");
        }
        for stdout in ["", "uid=00(root) gid=0", "uid=01", "uid=2000(shell) euid=0(root)", "euid=0(root)",
            "notuid=0", "notice uid=0", "uid=0root", "uid=0()", "uid=0(root)\nError: denied"] {
            assert!(!root_id_succeeded(&result(true, 0, stdout)), "{stdout}");
        }
        assert!(!root_id_succeeded(&result(false, 1, "uid=0(root)")));
        assert!(!root_id_succeeded(&result(true, 1, "uid=0(root)")));
    }

    #[test]
    fn observational_device_info_defaults_never_claim_checked_root() {
        let value = serde_json::to_value(DeviceInfo::default()).unwrap();
        assert_eq!(value["root"], false);
        assert_eq!(value["root_checked"], false);
        let status = serde_json::to_value(VmBootStatus { boot_completed: false, serial: "emulator-5554".into() }).unwrap();
        assert_eq!(status["boot_completed"], false);
        assert_eq!(status["serial"], "emulator-5554");
        assert_eq!(status.as_object().unwrap().len(), 2);
    }
}

/// Android application IDs use dot-separated ASCII segments, beginning with a letter.
/// Reject flags, whitespace, shell metacharacters, Unicode and empty/oversized segments.
pub(crate) fn validate_launch_package(package_name: &str) -> Result<()> {
    let mut segments = 0;
    let valid = !package_name.is_empty() && package_name.len() <= 255
        && package_name.split('.').all(|segment| {
            segments += 1;
            let mut bytes = segment.bytes();
            bytes.next().map(|byte| byte.is_ascii_alphabetic()).unwrap_or(false)
                && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        });
    if !valid || segments < 2 {
        return Err(Error::Other("Android包名需为至少两段ASCII标识符，每段以字母开头，仅含字母、数字和下划线".into()));
    }
    Ok(())
}

fn valid_activity_class(class_name: &str) -> bool {
    let name = class_name.strip_prefix('.').unwrap_or(class_name);
    !name.is_empty() && name.len() <= 1024 && name.split('.').all(|segment| {
        let mut bytes = segment.bytes();
        bytes.next().map(|byte| byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$').unwrap_or(false)
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$')
    })
}

/// resolve-activity --brief may precede its one flattened component with priority metadata.
fn parse_launcher_component(package_name: &str, output: &str) -> Result<String> {
    validate_launch_package(package_name)?;
    let mut component = None;
    for line in output.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if line.starts_with("priority=") && !line.contains('/') { continue; }
        let (resolved_package, class_name) = line.split_once('/')
            .ok_or_else(|| Error::Other("目标包未解析到唯一Launcher Activity".into()))?;
        if component.is_some() || resolved_package != package_name || !valid_activity_class(class_name) {
            return Err(Error::Other("Launcher组件无效、歧义或归属与目标包不符".into()));
        }
        component = Some(line.to_string());
    }
    component.ok_or_else(|| Error::Other("目标包没有可启动的Launcher Activity".into()))
}

/// am start can exit 0 while reporting an Android error; require its affirmative status.
fn launcher_start_succeeded(result: &CmdResult) -> bool {
    if !result.ok { return false; }
    let output = result.output().to_ascii_lowercase();
    let failed = output.lines().map(str::trim).any(|line| {
        line.starts_with("error:") || line.starts_with("error type ") || line.starts_with("exception ")
            || (line.starts_with("java.") && line.contains("exception"))
            || line.contains("permission denial") || line.contains("permission denied")
            || line.contains("does not exist") || line.contains("unable to resolve")
            || line.contains("not allowed") || line.contains("not exported")
            || (line.starts_with("status:") && line != "status: ok")
    });
    !failed && result.stdout.lines().any(|line| line.trim().eq_ignore_ascii_case("Status: ok"))
}

fn png_crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

/// A failed/truncated adb stream must never be presented as a captured PNG.
fn validate_guest_png(bytes: &[u8]) -> Result<()> {
    let invalid = || Error::Other("Guest screencap返回无效或截断PNG".into());
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") { return Err(invalid()); }
    let mut offset = 8usize;
    let mut header = false;
    let mut image = false;
    while offset < bytes.len() {
        if bytes.len() - offset < 12 { return Err(invalid()); }
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().map_err(|_| invalid())?) as usize;
        let end = offset.checked_add(12).and_then(|value| value.checked_add(length)).ok_or_else(invalid)?;
        if end > bytes.len() { return Err(invalid()); }
        let kind = &bytes[offset + 4..offset + 8];
        let expected_crc = u32::from_be_bytes(bytes[end - 4..end].try_into().map_err(|_| invalid())?);
        if png_crc32(&bytes[offset + 4..end - 4]) != expected_crc { return Err(invalid()); }
        if !header {
            if kind != b"IHDR" || length != 13 { return Err(invalid()); }
            let width = u32::from_be_bytes(bytes[offset + 8..offset + 12].try_into().map_err(|_| invalid())?);
            let height = u32::from_be_bytes(bytes[offset + 12..offset + 16].try_into().map_err(|_| invalid())?);
            if width == 0 || height == 0 || width > 32768 || height > 32768 { return Err(invalid()); }
            header = true;
        } else if kind == b"IHDR" { return Err(invalid()); }
        if kind == b"IDAT" && length > 0 { image = true; }
        if kind == b"IEND" {
            return if length == 0 && image && end == bytes.len() { Ok(()) } else { Err(invalid()) };
        }
        offset = end;
    }
    Err(invalid())
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    // Pure parsing fixtures only: no SDK, ADB, emulator, files, or processes.
    #[test]
    fn package_names_are_strict_ascii_segments() {
        for package in ["com.example", "com.example_app", "org.example.app2", "io.Example"] {
            assert!(validate_launch_package(package).is_ok(), "{package}");
        }
        for package in ["", "android", ".com.app", "com.app.", "com..app", "com.2app", "com._app",
            "-p", "com.app;id", "com.app$(id)", "com.app`id`", "com.app app", "com.app\n", "com.应用", "com.app/Activity"] {
            assert!(validate_launch_package(package).is_err(), "{package:?}");
        }
        assert!(validate_launch_package(&format!("com.{}", "x".repeat(252))).is_err());
    }

    #[test]
    fn launcher_component_is_unique_valid_and_owned_by_requested_package() {
        for component in ["com.example/.MainActivity", "com.example/com.example.MainActivity", "com.example/.Main$Nested"] {
            assert_eq!(parse_launcher_component("com.example", component).unwrap(), component);
        }
        assert_eq!(parse_launcher_component("com.example", "priority=0 preferredOrder=0 match=0x108000 specificIndex=-1 isDefault=true\ncom.example/.Main\n").unwrap(), "com.example/.Main");
        for output in ["", "No activity found", "com.other/.Main", "com.example/", "com.example/..Main",
            "com.example/.Main;id", "com.example/.Main'", "com.example/.Main/Extra", "com.example/.主界面",
            "com.example/.One\ncom.example/.Two", "Error: bad intent\ncom.example/.Main"] {
            assert!(parse_launcher_component("com.example", output).is_err(), "{output:?}");
        }
    }

    #[test]
    fn activity_manager_text_errors_are_not_successful_exit_codes() {
        let result = |ok, stdout: &str, stderr: &str| CmdResult {
            ok, stdout: stdout.into(), stderr: stderr.into(), code: if ok { 0 } else { 1 },
        };
        assert!(launcher_start_succeeded(&result(true, "Starting: Intent { ... }\nStatus: ok\nActivity: com.example/.Main\nComplete", "")));
        assert!(launcher_start_succeeded(&result(true, "Status: ok\nActivity: com.example/.ErrorActivity", "")));
        assert!(launcher_start_succeeded(&result(true, "Warning: Activity not started, intent has been delivered to currently running top-most instance.\nStatus: ok", "")));
        assert!(!launcher_start_succeeded(&result(false, "Status: ok", "")));
        assert!(!launcher_start_succeeded(&result(true, "Starting: Intent { ... }", "")));
        for output in ["Error type 3\nError: Activity class does not exist.", "Status: timeout", "Status: canceled",
            "Status: ok\nError: Permission Denial", "Status: ok\njava.lang.SecurityException"] {
            assert!(!launcher_start_succeeded(&result(true, output, "")), "{output}");
        }
        assert!(!launcher_start_succeeded(&result(true, "Status: ok", "Error: permission denied")));
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(&(data.len() as u32).to_be_bytes());
        output.extend_from_slice(kind);
        output.extend_from_slice(data);
        output.extend_from_slice(&png_crc32(&output[4..]).to_be_bytes());
        output
    }

    fn fixture() -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let header = [0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0];
        png.extend(chunk(b"IHDR", &header));
        // Valid zlib stream for one transparent RGBA pixel and its filter byte.
        png.extend(chunk(b"IDAT", &[0x78, 0x9c, 0x63, 0x60, 0x00, 0x02, 0x00, 0x00, 0x05, 0x00, 0x01]));
        png.extend(chunk(b"IEND", &[]));
        png
    }

    #[test]
    fn png_capture_preserves_binary_and_checks_complete_chunks() {
        let png = fixture();
        assert!(validate_guest_png(&png).is_ok());
        assert_eq!(png_crc32(b"123456789"), 0xcbf4_3926);
        for end in 0..png.len() { assert!(validate_guest_png(&png[..end]).is_err()); }
        let mut corrupted = png.clone(); corrupted[45] ^= 1;
        assert!(validate_guest_png(&corrupted).is_err());
        let mut trailing = png; trailing.push(0);
        assert!(validate_guest_png(&trailing).is_err());
        assert!(validate_guest_png(b"error: device offline").is_err());
    }

    #[test]
    fn png_capture_rejects_missing_pixels_or_header() {
        let mut no_pixels = b"\x89PNG\r\n\x1a\n".to_vec();
        no_pixels.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]));
        no_pixels.extend(chunk(b"IEND", &[]));
        assert!(validate_guest_png(&no_pixels).is_err());
        let mut no_header = b"\x89PNG\r\n\x1a\n".to_vec();
        no_header.extend(chunk(b"IDAT", &[1]));
        no_header.extend(chunk(b"IEND", &[]));
        assert!(validate_guest_png(&no_header).is_err());
    }
}
