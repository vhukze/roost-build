//! Roost 后端核心类型

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ------------------------------------------------------------------ 错误

#[derive(Debug)]
pub enum Error {
    PathNotFound(PathBuf),
    Io(std::io::Error),
    Json(serde_json::Error),
    Command(String),
    QemuMissing,
    ImagesMissing(String),
    DeviceNotConnected,
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

// Tauri 需要错误可序列化
impl serde::Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::PathNotFound(p) => write!(f, "路径不存在: {}", p.display()),
            Error::Io(e) => write!(f, "文件操作失败: {e}"),
            Error::Json(e) => write!(f, "JSON 解析失败: {e}"),
            Error::Command(s) => write!(f, "命令执行失败: {s}"),
            Error::QemuMissing => write!(f, "QEMU 未安装，请先运行 setup/01-install-qemu.ps1"),
            Error::ImagesMissing(s) => write!(f, "基础镜像不完整，缺失: {s}"),
            Error::DeviceNotConnected => write!(f, "设备未连接"),
            Error::Other(s) => write!(f, "{s}"),
        }
    }
}

// ------------------------------------------------------------------ 路径

/// 项目根目录（Roost 目录）
pub fn project_root() -> PathBuf {
    // 开发时: app/src-tauri -> 上两级是 Roost
    // 发布时: exe 所在目录
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // 优先检查 exe 同级是否有 runtime 目录
            if dir.join("runtime").exists() || !dir.ancestors().any(|p| p.file_name().map(|n| n == "target").unwrap_or(false)) {
                return dir.to_path_buf();
            }
        }
    }

    // 回退到源码位置
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn runtime_dir() -> PathBuf {
    project_root().join("runtime")
}
pub fn images_dir() -> PathBuf {
    project_root().join("images")
}
pub fn vms_dir() -> PathBuf {
    project_root().join("data").join("vms")
}
pub fn overlays_dir() -> PathBuf {
    images_dir().join("overlays")
}
pub fn base_images_dir() -> PathBuf {
    images_dir().join("base")
}
pub fn compat_dir() -> PathBuf {
    project_root().join("data").join("compat")
}

pub fn qemu_exe() -> PathBuf {
    runtime_dir().join("qemu").join("qemu-system-aarch64.exe")
}
pub fn qemu_img_exe() -> PathBuf {
    runtime_dir().join("qemu").join("qemu-img.exe")
}
pub fn adb_exe() -> PathBuf {
    runtime_dir().join("platform-tools").join("adb.exe")
}
pub fn scrcpy_exe() -> PathBuf {
    runtime_dir().join("scrcpy").join("scrcpy.exe")
}

// ------------------------------------------------------------------ 虚拟机配置

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmConfig {
    pub id: String,
    pub name: String,
    #[serde(default = "default_memory")]
    pub memory_mb: u32,
    #[serde(default = "default_cores")]
    pub cores: u32,
    #[serde(default = "default_base_image")]
    pub base_image: String,
    #[serde(default = "default_root")]
    pub root_manager: String,
    #[serde(default = "default_disk")]
    pub disk: String,
    #[serde(default = "default_port")]
    pub adb_port: u16,
    #[serde(default = "default_accel")]
    pub accel: String,
    #[serde(default = "default_tb")]
    pub tb_size_mb: u32,
    #[serde(default = "default_gpu")]
    pub gpu: String,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub lsp_version: Option<String>,
    // Missing backend means an existing, legacy QEMU VM. New VMs opt in explicitly.
    #[serde(default = "default_backend")]
    pub backend: String,
    #[serde(default)]
    pub sdk_path: String,
    #[serde(default)]
    pub avd_name: String,
    #[serde(default)]
    pub avd_home: String,
    #[serde(default)]
    pub emulator_image_dir: String,
    #[serde(default = "default_console_port")]
    pub console_port: u16,
    /// Prepared input only: copied to this VM's boot directory, never edited in place.
    #[serde(default)]
    pub prepared_ramdisk: String,
    #[serde(default)]
    pub prepared_root: String,
    #[serde(default = "default_magisk_path")]
    pub magisk_path: String,
    #[serde(default)]
    pub root_applied: Option<String>,
    #[serde(default)]
    pub lsp_applied: Option<String>,
    #[serde(default)]
    pub lsp_state: String,
    #[serde(default)]
    pub pending_restart: bool,
    #[serde(default)]
    pub module_pending_boot_id: Option<String>,
}

fn default_backend() -> String { "legacy-qemu".into() }
fn default_console_port() -> u16 { 5554 }
fn default_magisk_path() -> String { "/debug_ramdisk/magisk".into() }
fn default_memory() -> u32 { 6144 }
fn default_cores() -> u32 { 8 }
fn default_base_image() -> String { "aosp14-arm64".into() }
fn default_root() -> String { "none".into() }
fn default_disk() -> String { "disk.qcow2".into() }
fn default_port() -> u16 { 5555 }
fn default_accel() -> String { "tcg".into() }
fn default_tb() -> u32 { 1536 }
fn default_gpu() -> String { "virtio".into() }

pub fn validate_component(value: &str) -> Result<()> {
    if value.is_empty() || value == "." || value == ".."
        || !value.chars().all(|c| c.is_ascii_alphanumeric() || "._@+-".contains(c)) {
        return Err(Error::Other("标识含无效路径字符".into()));
    }
    Ok(())
}

impl VmConfig {
    pub fn validate(&self) -> Result<()> {
        validate_component(&self.id)?;
        validate_component(&self.disk)?;
        if self.backend != "legacy-qemu" && self.backend != "android-emulator" {
            return Err(Error::Other("未知虚拟机后端".into()));
        }
        if self.console_port < 5554 || self.console_port > 5682 || self.console_port % 2 != 0 {
            return Err(Error::Other("Emulator console 端口须为 5554..5682 的偶数".into()));
        }
        if self.adb_port < 1024 { return Err(Error::Other("ADB端口无效".into())); }
        if !(256..=32768).contains(&self.memory_mb) || !(1..=32).contains(&self.cores) {
            return Err(Error::Other("内存须256..32768MB，核心须1..32".into()));
        }
        if self.root_manager != "none" {
            let version = self.root_manager.strip_prefix("magisk@")
                .ok_or_else(|| Error::Other("当前镜像仅支持 none / Magisk；其他方案需匹配内核".into()))?;
            validate_component(version)?;
        }
        if let Some(lsp) = &self.lsp_version {
            if lsp != "none" { validate_component(lsp)?; }
        }
        if !self.magisk_path.starts_with('/') || !self.magisk_path.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c)) {
            return Err(Error::Other("Magisk 路径须为无 shell 字符的绝对设备路径".into()));
        }
        Ok(())
    }

    pub fn serial(&self) -> Result<String> {
        self.validate()?;
        Ok(if self.backend == "android-emulator" {
            format!("emulator-{}", self.console_port)
        } else { format!("127.0.0.1:{}", self.adb_port) })
    }

    pub fn lsp_desired(&self) -> &str {
        self.lsp_version.as_deref().filter(|v| !v.is_empty()).unwrap_or("none")
    }

    pub fn dir(&self) -> PathBuf {
        vms_dir().join(&self.id)
    }

    pub fn disk_path(&self) -> PathBuf {
        self.dir().join(&self.disk)
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir().join("config.json")
    }

    pub fn save(&self) -> Result<()> {
        self.validate()?;
        std::fs::create_dir_all(self.dir())?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(self.config_path(), json)?;
        Ok(())
    }

    pub fn load(id: &str) -> Result<Self> {
        validate_component(id)?;
        let path = vms_dir().join(id).join("config.json");
        if !path.exists() {
            return Err(Error::PathNotFound(path));
        }
        let text = std::fs::read_to_string(&path)?;
        let config: Self = serde_json::from_str(&text)?;
        if config.id != id { return Err(Error::Other("VM目录与配置ID不一致".into())); }
        Ok(config)
    }

    pub fn list() -> Vec<Self> {
        let dir = vms_dir();
        if !dir.exists() {
            return Vec::new();
        }

        let mut vms = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let cfg = entry.path().join("config.json");
                if cfg.exists() {
                    if let Ok(text) = std::fs::read_to_string(&cfg) {
                        if let Ok(vm) = serde_json::from_str::<VmConfig>(&text) {
                            vms.push(vm);
                        }
                    }
                }
            }
        }
        vms.sort_by(|a, b| a.id.cmp(&b.id));
        vms
    }

    /// 生成 QEMU 命令行
    pub fn build_command(&self, headless: bool) -> Vec<String> {
        let base = base_images_dir();
        let tb = self.tb_size_mb.max(256).min(2048);

        let mut args: Vec<String> = vec![
            qemu_exe().to_string_lossy().into(),
            "-machine".into(), "virt,gic-version=3".into(),
            "-cpu".into(), "cortex-a72".into(),
            "-smp".into(), self.cores.to_string(),
            "-m".into(), self.memory_mb.to_string(),
            "-accel".into(), format!("tcg,thread=multi,tb-size={tb}"),
            "-kernel".into(), base.join("kernel").to_string_lossy().into(),
            "-initrd".into(), self.dir().join("boot").join("ramdisk.img").to_string_lossy().into(),
            "-append".into(),
            concat!(
                "console=ttyAMA0 ",
                "androidboot.hardware=ranchu ",
                "androidboot.selinux=permissive ",
                "androidboot.console=ttyAMA0 ",
                "root=/dev/vda2 rw"
            ).into(),
            "-drive".into(),
            format!("file={},if=virtio,format=qcow2,readonly=on",
                    base.join("system.qcow2").to_string_lossy()),
            "-drive".into(),
            format!("file={},if=virtio,format=qcow2,readonly=on",
                    base.join("vendor.qcow2").to_string_lossy()),
            "-drive".into(),
            format!("file={},if=virtio,format=qcow2,cache=writeback",
                    self.disk_path().to_string_lossy()),
            "-device".into(), "virtio-net-pci,netdev=n0".into(),
            "-netdev".into(),
            format!("user,id=n0,hostfwd=tcp::{}-:5555", self.adb_port),
        ];

        if headless {
            args.extend(["-nographic".into(), "-serial".into(), "mon:stdio".into()]);
        } else {
            args.extend([
                "-device".into(), "virtio-gpu-pci".into(),
                "-device".into(), "virtio-keyboard-pci".into(),
                "-device".into(), "virtio-tablet-pci".into(),
                "-display".into(), "gtk".into(),
            ]);
        }

        args
    }
}

// ------------------------------------------------------------------ Overlay

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayManifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub license: String,
    #[serde(default = "default_android_min")]
    pub android_min: u32,
    #[serde(default = "default_android_max")]
    pub android_max: u32,
    #[serde(default)]
    pub requires_kernel_patch: bool,
    #[serde(default)]
    pub zygisk_provider: bool,
    #[serde(default)]
    pub checks: OverlayChecks,
    #[serde(default)]
    pub symlinks: Vec<Symlink>,
}

fn default_android_min() -> u32 { 8 }
fn default_android_max() -> u32 { 15 }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OverlayChecks {
    #[serde(default)]
    pub su_path: Option<String>,
    #[serde(default)]
    pub verify_cmd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symlink {
    pub path: String,
    pub target: String,
}

/// 带运行时状态的 overlay 信息
#[derive(Debug, Clone, Serialize)]
pub struct OverlayInfo {
    #[serde(flatten)]
    pub manifest: OverlayManifest,
    pub has_binary: bool,
    pub binary_count: usize,
    pub total_bytes: u64,
    pub zygisk: bool,
}

impl OverlayManifest {
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("manifest.json");
        if !path.exists() {
            return Err(Error::PathNotFound(path));
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn is_ready(&self) -> bool {
        if self.id == "none" {
            return true;
        }
        let bin = overlays_dir().join(&self.id).join("files").join("system").join("bin");
        bin.exists() && std::fs::read_dir(&bin).map(|mut d| d.next().is_some()).unwrap_or(false)
    }

    pub fn to_info(&self) -> OverlayInfo {
        let dir = overlays_dir().join(&self.id).join("files");
        let mut count = 0usize;
        let mut total = 0u64;

        if dir.exists() {
            for entry in walkdir::WalkDir::new(&dir).into_iter().flatten() {
                if entry.file_type().is_file() {
                    count += 1;
                    total += entry.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        }

        OverlayInfo {
            manifest: self.clone(),
            has_binary: self.is_ready(),
            binary_count: count,
            total_bytes: total,
            zygisk: self.zygisk_provider,
        }
    }
}

pub fn list_overlays() -> Vec<OverlayInfo> {
    let dir = overlays_dir();
    if !dir.exists() {
        return Vec::new();
    }

    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Ok(m) = OverlayManifest::load(&entry.path()) {
                    out.push(m.to_info());
                }
            }
        }
    }
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    out
}

// ------------------------------------------------------------------ 环境状态

#[derive(Debug, Clone, Serialize)]
pub struct EnvStatus {
    pub emulator: bool,
    pub acceleration_note: String,
    pub qemu: bool,
    pub qemu_img: bool,
    pub adb: bool,
    pub scrcpy: bool,
    pub images_ready: bool,
    pub missing_images: Vec<String>,
    pub overlay_count: usize,
    pub project_root: String,
}

impl EnvStatus {
    pub fn check() -> Self {
        let base = base_images_dir();
        let needed = ["kernel", "ramdisk.img", "system.qcow2", "vendor.qcow2"];
        let missing: Vec<String> = needed
            .iter()
            .filter(|n| !base.join(n).exists())
            .map(|s| s.to_string())
            .collect();

        Self {
            emulator: std::env::var_os("ANDROID_HOME").or_else(|| std::env::var_os("ANDROID_SDK_ROOT"))
                .map(|p| PathBuf::from(p).join("emulator/emulator.exe").exists()).unwrap_or(false),
            acceleration_note: "按 VM 的 SDK 执行 -accel-check；仅检测，不操作驱动".into(),
            qemu: qemu_exe().exists(),
            qemu_img: qemu_img_exe().exists(),
            adb: adb_exe().exists(),
            scrcpy: scrcpy_exe().exists(),
            images_ready: missing.is_empty(),
            missing_images: missing,
            overlay_count: list_overlays().len(),
            project_root: project_root().to_string_lossy().into(),
        }
    }

    pub fn require_qemu(&self) -> Result<()> {
        if !self.qemu {
            return Err(Error::QemuMissing);
        }
        Ok(())
    }

    pub fn require_images(&self) -> Result<()> {
        if !self.images_ready {
            return Err(Error::ImagesMissing(self.missing_images.join(", ")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod managed_config_tests {
    use super::*;
    #[test]
    fn missing_backend_keeps_legacy_config() {
        let c: VmConfig = serde_json::from_str(r#"{"id":"vm1","name":"old"}"#).unwrap();
        assert_eq!(c.backend, "legacy-qemu");
        assert_eq!(c.serial().unwrap(), "127.0.0.1:5555");
        assert_eq!(c.magisk_path, "/debug_ramdisk/magisk");
    }
    #[test]
    fn emulator_serial_is_derived_not_arbitrary() {
        let mut c: VmConfig = serde_json::from_str(r#"{"id":"vm2","name":"new"}"#).unwrap();
        c.backend = "android-emulator".into(); c.console_port = 5560;
        assert_eq!(c.serial().unwrap(), "emulator-5560");
        c.console_port = 5561; assert!(c.serial().is_err());
    }
    #[test]
    fn boot_selection_is_explicit_and_no_paths_in_identifiers() {
        let mut c: VmConfig = serde_json::from_str(r#"{"id":"vm3","name":"new"}"#).unwrap();
        c.root_manager = "magisk@28.0".into(); assert!(c.validate().is_ok());
        c.root_manager = "ksu@1.0".into(); assert!(c.validate().is_err());
        c.id = "../physical-device".into(); assert!(c.validate().is_err());
    }
    #[test]
    fn legacy_launch_uses_private_ramdisk() {
        let c: VmConfig = serde_json::from_str(r#"{"id":"vm4","name":"old"}"#).unwrap();
        let args = c.build_command(true);
        let i = args.iter().position(|v| v == "-initrd").unwrap();
        assert_eq!(args[i+1], c.dir().join("boot/ramdisk.img").to_string_lossy());
    }
}
