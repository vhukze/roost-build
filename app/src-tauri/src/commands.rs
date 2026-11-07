//! Tauri 命令层
//!
//! 所有前端可调用的接口都在这里注册。

use crate::adb::Adb;
use crate::compat::{AppRecord, Matrix};
use crate::diagnose::{self, Report};
use crate::root::{SwitchResult, VerifyResult};
use crate::types::*;
use crate::vm::{LaunchResult, VmManager};
use std::sync::Arc;
use tauri::State;

// ------------------------------------------------------------------ 应用状态

pub struct AppState {
    pub vm_manager: VmManager,
    pub matrix: Arc<Matrix>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            vm_manager: VmManager::new(),
            matrix: Arc::new(Matrix::new()),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------------ 环境

#[tauri::command]
pub fn get_env_status() -> EnvStatus {
    EnvStatus::check()
}

#[tauri::command]
pub fn get_project_root() -> String {
    project_root().to_string_lossy().into()
}

// ------------------------------------------------------------------ 虚拟机

#[tauri::command]
pub fn list_vms() -> Vec<VmConfig> {
    VmConfig::list()
}

#[tauri::command]
pub fn get_vm(vm_id: String) -> Result<VmConfig> {
    VmConfig::load(&vm_id)
}

#[tauri::command]
pub fn save_vm(state: State<'_, AppState>, mut config: VmConfig) -> Result<()> {
    let _operation = state.vm_manager.operation(&config.id)?;
    config.validate()?;
    let previous = VmConfig::load(&config.id)?;
    if state.vm_manager.is_running(&config.id)
        && (previous.memory_mb != config.memory_mb || previous.cores != config.cores || previous.backend != config.backend || previous.adb_port != config.adb_port || previous.console_port != config.console_port
            || previous.sdk_path != config.sdk_path || previous.avd_name != config.avd_name || previous.avd_home != config.avd_home
            || previous.emulator_image_dir != config.emulator_image_dir || previous.magisk_path != config.magisk_path) {
        return Err(Error::Other("运行中仅可保存下次启动的Root/LSPosed选择；设备身份配置请停机后修改".into()));
    }
    config.root_applied = previous.root_applied;
    config.lsp_applied = previous.lsp_applied;
    config.lsp_state = previous.lsp_state;
    config.module_pending_boot_id = previous.module_pending_boot_id;
    config.pending_restart = previous.pending_restart || config.root_applied.as_deref() != Some(config.root_manager.as_str())
        || config.lsp_applied.as_deref() != Some(config.lsp_desired())
        || previous.memory_mb != config.memory_mb || previous.cores != config.cores
        || previous.prepared_ramdisk != config.prepared_ramdisk || previous.prepared_root != config.prepared_root
        || previous.backend != config.backend || previous.sdk_path != config.sdk_path || previous.avd_home != config.avd_home
        || previous.avd_name != config.avd_name || previous.emulator_image_dir != config.emulator_image_dir;
    config.save()
}

#[tauri::command]
pub fn delete_vm(state: State<'_, AppState>, vm_id: String) -> Result<()> {
    let _operation = state.vm_manager.operation(&vm_id)?;
    validate_component(&vm_id)?;
    if state.vm_manager.is_running(&vm_id) { return Err(Error::Other("请先停止该 VM 再删除".into())); }
    let dir = vms_dir().join(&vm_id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

#[tauri::command]
pub fn create_vm(vm_id: String, name: String) -> Result<VmConfig> {
    validate_component(&vm_id)?;
    if vms_dir().join(&vm_id).exists() { return Err(Error::Other("VM 标识已存在".into())); }
    let existing = VmConfig::list();
    let console_port = (5554u16..=5682).step_by(2).find(|p| existing.iter().all(|c| c.console_port != *p))
        .ok_or_else(|| Error::Other("无可用 Emulator 端口".into()))?;
    let config = VmConfig {
        id: vm_id.clone(), name,
        backend: "android-emulator".into(), accel: "aehd".into(),
        console_port, adb_port: console_port + 1,
        sdk_path: std::env::var("ANDROID_HOME").or_else(|_| std::env::var("ANDROID_SDK_ROOT")).unwrap_or_default(),
        created: crate::now_string(), pending_restart: true,
        ..serde_json::from_str(r#"{"id":"","name":""}"#)?
    };

    config.save()?;
    Ok(config)
}

#[tauri::command]
pub async fn launch_vm(
    state: State<'_, AppState>,
    vm_id: String,
    headless: Option<bool>,
) -> Result<LaunchResult> {
    state
        .vm_manager
        .launch(&vm_id, headless.unwrap_or(false))
        .await
}

#[tauri::command]
pub async fn stop_vm(state: State<'_, AppState>, vm_id: String) -> Result<String> {
    state.vm_manager.stop(&vm_id).await
}

#[tauri::command]
pub fn vm_running(state: State<'_, AppState>, vm_id: String) -> bool {
    state.vm_manager.is_running(&vm_id)
}

#[tauri::command]
pub fn running_vms(state: State<'_, AppState>) -> Vec<String> {
    state.vm_manager.running_ids()
}

#[tauri::command]
pub fn vm_disk_info(vm_id: String) -> Option<u64> {
    crate::vm::disk_info(&vm_id)
}

#[tauri::command]
pub fn build_vm_command(vm_id: String, headless: Option<bool>) -> Result<Vec<String>> {
    let config = VmConfig::load(&vm_id)?;
    if config.backend == "android-emulator" {
        crate::managed::emulator_command(&config, headless.unwrap_or(false))
    } else { Ok(config.build_command(headless.unwrap_or(false))) }
}

fn validate_package_name(package: &str) -> Result<()> {
    if package.is_empty() || package.starts_with('-') || !package.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
        return Err(Error::Other("Android包名含无效字符".into()));
    }
    Ok(())
}

#[tauri::command]
pub fn list_prepared_boots(vm_id: String) -> Result<Vec<crate::managed::PreparedBoot>> {
    let c = VmConfig::load(&vm_id)?;
    c.validate()?;
    crate::managed::list_prepared_boots(&c)
}

// ------------------------------------------------------------------ Overlay

#[tauri::command]
pub fn list_overlays() -> Vec<OverlayInfo> {
    crate::types::list_overlays()
}

#[tauri::command]
pub fn get_overlay(overlay_id: String) -> Result<OverlayInfo> {
    let dir = overlays_dir().join(&overlay_id);
    let manifest = OverlayManifest::load(&dir)?;
    Ok(manifest.to_info())
}

// ------------------------------------------------------------------ ADB

#[tauri::command]
pub async fn adb_connect(state: State<'_, AppState>, vm_id: String) -> Result<bool> {
    let _operation = state.vm_manager.operation(&vm_id)?;
    Ok(state.vm_manager.adb_for_vm(&vm_id)?.connect().await)
}

#[tauri::command]
pub async fn adb_device_info(state: State<'_, AppState>, vm_id: String) -> Result<crate::adb::DeviceInfo> {
    let _operation = state.vm_manager.operation(&vm_id)?;
    Ok(state.vm_manager.adb_for_vm(&vm_id)?.device_info().await)
}

#[tauri::command]
pub async fn adb_list_packages(state: State<'_, AppState>, vm_id: String) -> Result<Vec<String>> {
    let _operation = state.vm_manager.operation(&vm_id)?;
    let result = state.vm_manager.adb_for_vm(&vm_id)?.shell("pm list packages -3").await?;
    if !result.ok { return Err(Error::Command(result.output())); }
    Ok(result.stdout.lines().filter_map(|line| line.strip_prefix("package:")).map(|v| v.trim().to_string()).collect())
}

#[tauri::command]
pub async fn adb_install(state: State<'_, AppState>, vm_id: String, apk: String) -> Result<crate::adb::CmdResult> {
    let _operation = state.vm_manager.operation(&vm_id)?;
    if !apk.to_lowercase().ends_with(".apk") { return Err(Error::Other("APK 安装器仅接收 .apk；ZIP 请用 Magisk 安装器".into())); }
    let result = state.vm_manager.adb_for_vm(&vm_id)?.install(&apk).await?;
    if !result.ok { return Err(Error::Command(result.output())); }
    Ok(result)
}

#[tauri::command]
pub async fn adb_uninstall(state: State<'_, AppState>, vm_id: String, package: String) -> Result<crate::adb::CmdResult> {
    validate_package_name(&package)?;
    let _operation = state.vm_manager.operation(&vm_id)?;
    state.vm_manager.adb_for_vm(&vm_id)?.uninstall(&package).await
}

#[tauri::command]
pub async fn adb_logcat(state: State<'_, AppState>, vm_id: String, lines: Option<u32>) -> Result<String> {
    let _operation = state.vm_manager.operation(&vm_id)?;
    Ok(state.vm_manager.adb_for_vm(&vm_id)?.logcat(lines.unwrap_or(300), "main").await)
}

#[tauri::command]
pub async fn adb_crash_log(state: State<'_, AppState>, vm_id: String, lines: Option<u32>) -> Result<String> {
    let _operation = state.vm_manager.operation(&vm_id)?;
    Ok(state.vm_manager.adb_for_vm(&vm_id)?.crash_log(lines.unwrap_or(200)).await)
}

// Legacy live overlay mutation is intentionally not used for prepared boot images.
#[tauri::command]
pub async fn switch_root(state: State<'_, AppState>, vm_id: String, target: String, previous: String) -> Result<SwitchResult> {
    let _ = (state, vm_id, target, previous);
    Err(Error::Other("请在 VM 配置选择 Magisk 版本及匹配的 prepared ramdisk，停机后重新启动；旧Overlay热切换已停用".into()))
}

#[tauri::command]
pub async fn verify_root(state: State<'_, AppState>, vm_id: String, overlay_id: String) -> Result<VerifyResult> {
    let _operation = state.vm_manager.operation(&vm_id)?;
    let c = state.vm_manager.running_config(&vm_id)?;
    if c.root_manager != overlay_id { return Err(Error::Other("请求版本与运行中的VM版本不一致".into())); }
    let r = crate::managed::verify_vm(&state.vm_manager.adb_for_vm(&vm_id)?, &c).await?;
    Ok(VerifyResult { ok: r.root_verified, message: r.message })
}

#[tauri::command]
pub async fn check_root(state: State<'_, AppState>, vm_id: String) -> Result<bool> {
    let _operation = state.vm_manager.operation(&vm_id)?;
    Ok(state.vm_manager.adb_for_vm(&vm_id)?.check_root().await)
}

#[tauri::command]
pub async fn initialize_vm_magisk(state: State<'_, AppState>, vm_id: String) -> Result<crate::adb::CmdResult> {
    let _operation = state.vm_manager.operation(&vm_id)?;
    let c = state.vm_manager.running_config(&vm_id)?;
    crate::managed::initialize_magisk(&state.vm_manager.adb_for_vm(&vm_id)?, &c).await
}

#[tauri::command]
pub async fn install_magisk_module(state: State<'_, AppState>, vm_id: String, path: String) -> Result<crate::managed::ModuleInstallResult> {
    let _operation = state.vm_manager.operation(&vm_id)?;
    let c = state.vm_manager.running_config(&vm_id)?;
    if !c.root_manager.starts_with("magisk@") { return Err(Error::Other("当前运行的VM未选择Magisk".into())); }
    let adb = state.vm_manager.adb_for_vm(&vm_id)?;
    let boot = adb.shell("cat /proc/sys/kernel/random/boot_id").await?;
    if !boot.ok || boot.stdout.trim().is_empty() { return Err(Error::Other("读取目标VM boot_id失败".into())); }
    let result = crate::managed::install_zip(&adb, &c, &path).await?;
    let mut saved = VmConfig::load(&vm_id)?;
    saved.module_pending_boot_id = Some(boot.stdout.trim().into());
    saved.pending_restart = true;
    saved.save()?;
    Ok(result)
}

#[tauri::command]
pub async fn apply_vm_lsposed(state: State<'_, AppState>, vm_id: String) -> Result<crate::managed::ModuleInstallResult> {
    let _operation = state.vm_manager.operation(&vm_id)?;
    let running = state.vm_manager.running_config(&vm_id)?;
    let c = VmConfig::load(&vm_id)?;
    if c.root_manager != running.root_manager || !running.root_manager.starts_with("magisk@") {
        return Err(Error::Other("请先用所选Magisk版本启动该VM".into()));
    }
    let adb = state.vm_manager.adb_for_vm(&vm_id)?;
    if !adb.is_booted().await || !adb.check_root().await { return Err(Error::Other("目标VM尚未完成启动或未取得root".into())); }
    let boot = adb.shell("cat /proc/sys/kernel/random/boot_id").await?;
    if !boot.ok || boot.stdout.trim().is_empty() { return Err(Error::Other("读取目标VM boot_id失败".into())); }
    if c.lsp_desired() == "none" {
        let result = adb.su_shell(r#"for d in /data/adb/modules/zygisk_lsposed /data/adb/modules/riru_lsposed /data/adb/modules_update/zygisk_lsposed /data/adb/modules_update/riru_lsposed; do [ ! -d "$d" ] || touch "$d/disable"; done"#).await?;
        if !result.ok { return Err(Error::Command(result.output())); }
        let mut saved = c; saved.pending_restart = true; saved.module_pending_boot_id = Some(boot.stdout.trim().into()); saved.save()?;
        return Ok(crate::managed::ModuleInstallResult { ok: true, stdout: result.stdout, stderr: result.stderr, code: result.code,
            needs_reboot: true, module_id: "lsposed-disabled".into() });
    }
    if c.lsp_desired().ends_with("-riru") {
        return Err(Error::Other("自动基线暂仅支持1.9.2 Zygisk；Riru依赖需手动准备".into()));
    }
    let zygisk = adb.su_shell(&format!(r#"{} --sqlite 'SELECT value FROM settings WHERE key="zygisk"'"#, c.magisk_path)).await?;
    if !zygisk.ok || !zygisk.stdout.contains("value=1") {
        return Err(Error::Other("请在该VM的Magisk管理器启用Zygisk并重启后再应用LSPosed".into()));
    }
    let path = crate::managed::lsposed_package(c.lsp_desired())?;
    let result = crate::managed::install_zip(&adb, &running, &path.to_string_lossy()).await?;
    let mut saved = c; saved.pending_restart = true; saved.module_pending_boot_id = Some(boot.stdout.trim().into()); saved.save()?;
    Ok(result)
}

// Desired values alone do not prove that the booted Root choice took effect.
// Keep a failed Root observation pending even when every configuration field matches.
fn runtime_pending_restart(desired: &VmConfig, running: &VmConfig, reboot_pending: bool, root_verified: bool) -> bool {
    reboot_pending || !root_verified
        || desired.root_applied.as_deref() != Some(desired.root_manager.as_str())
        || desired.memory_mb != running.memory_mb || desired.cores != running.cores
        || desired.root_manager != running.root_manager || desired.prepared_ramdisk != running.prepared_ramdisk
        || desired.prepared_root != running.prepared_root
        || desired.lsp_applied.as_deref() != Some(desired.lsp_desired())
}

#[tauri::command]
pub async fn verify_vm_runtime(state: State<'_, AppState>, vm_id: String) -> Result<crate::managed::VerifyVmResult> {
    let _operation = state.vm_manager.operation(&vm_id)?;
    let running = state.vm_manager.running_config(&vm_id)?;
    let mut desired = VmConfig::load(&vm_id)?;
    // Root must be verified against the booted choice, LSP against the latest explicitly applied selection.
    let mut expected = running.clone(); expected.lsp_version = desired.lsp_version.clone();
    let mut result = crate::managed::verify_vm(&state.vm_manager.adb_for_vm(&vm_id)?, &expected).await?;
    if result.root_verified { desired.root_applied = Some(running.root_manager.clone()); } else { desired.root_applied = None; }
    if result.lsp_verified || result.lsp_installed { desired.lsp_applied = Some(desired.lsp_desired().to_string()); } else { desired.lsp_applied = None; }
    desired.lsp_state = result.lsp_state.clone();
    let boot = state.vm_manager.adb_for_vm(&vm_id)?.shell("cat /proc/sys/kernel/random/boot_id").await?;
    let reboot_pending = desired.module_pending_boot_id.as_deref()
        .map(|id| !boot.ok || boot.stdout.trim().is_empty() || id == boot.stdout.trim()).unwrap_or(false);
    if !reboot_pending { desired.module_pending_boot_id = None; }
    result.pending_restart = runtime_pending_restart(&desired, &running, reboot_pending, result.root_verified);
    desired.pending_restart = result.pending_restart;
    desired.save()?;
    Ok(result)
}

#[tauri::command]
pub async fn diagnose_app(state: State<'_, AppState>, vm_id: String, package: String, wait_secs: Option<u64>) -> Result<Report> {
    validate_package_name(&package)?;
    let _operation = state.vm_manager.operation(&vm_id)?;
    let adb = state.vm_manager.adb_for_vm(&vm_id)?;
    Ok(diagnose::diagnose(&adb, &package, wait_secs.unwrap_or(30).clamp(1, 120)).await)
}

// ------------------------------------------------------------------ 兼容性矩阵

#[tauri::command]
pub fn list_compat(state: State<'_, AppState>) -> Vec<AppRecord> {
    state.matrix.list()
}

#[tauri::command]
pub fn upsert_compat(state: State<'_, AppState>, record: AppRecord) -> Result<()> {
    state.matrix.upsert(record)
}

#[tauri::command]
pub fn set_compat_state(
    state: State<'_, AppState>,
    package: String,
    state_name: String,
    issue: Option<String>,
) -> Result<()> {
    state
        .matrix
        .set_state(&package, &state_name, issue.as_deref().unwrap_or(""))
}

#[tauri::command]
pub fn remove_compat(state: State<'_, AppState>, package: String) -> Result<bool> {
    state.matrix.remove(&package)
}

#[tauri::command]
pub fn compat_stats(state: State<'_, AppState>) -> std::collections::HashMap<String, u32> {
    state.matrix.stats()
}

#[tauri::command]
pub fn export_compat_csv(state: State<'_, AppState>) -> Result<String> {
    let path = state.matrix.export_csv()?;
    Ok(path.to_string_lossy().into())
}

// ------------------------------------------------------------------ Root 下载

#[tauri::command]
pub async fn list_root_managers() -> Vec<crate::roots::RootManager> {
    crate::roots::list_managers().await
}

#[tauri::command]
pub async fn download_root(
    window: tauri::Window,
    manager: String,
    version: String,
    url: String,
    asset_name: String,
    source: String,
) -> Result<String> {
    use tauri::Emitter;

    let src = if source == "mirror" {
        crate::roots::DownloadSource::Mirror
    } else {
        crate::roots::DownloadSource::Direct
    };

    let w = window.clone();
    let mgr = manager.clone();
    let ver = version.clone();

    let result = crate::roots::download_version(
        &manager, &version, &url, &asset_name, src, move |p| {
            let _ = w.emit("root-download-progress", &p);
        },
    )
    .await;

    match &result {
        Ok(msg) => {
            let _ = window.emit(
                "root-download-progress",
                crate::roots::DownloadProgress {
                    manager: mgr,
                    version: ver,
                    downloaded: 0,
                    total: 0,
                    percent: 100.0,
                    status: "done".into(),
                    message: msg.clone(),
                },
            );
        }
        Err(e) => {
            let _ = window.emit(
                "root-download-progress",
                crate::roots::DownloadProgress {
                    manager: mgr,
                    version: ver,
                    downloaded: 0,
                    total: 0,
                    percent: 0.0,
                    status: "error".into(),
                    message: e.to_string(),
                },
            );
        }
    }

    result
}

#[tauri::command]
pub fn remove_root_version(manager: String, version: String) -> Result<()> {
    crate::roots::remove_version(&manager, &version)
}

#[tauri::command]
pub fn check_root_downloaded(manager: String, version: String) -> bool {
    crate::roots::is_downloaded(&manager, &version)
}

// ------------------------------------------------------------------ LSPosed

#[tauri::command]
pub async fn list_lsposed() -> crate::packages::PkgManager {
    crate::packages::list_lsposed().await
}

#[tauri::command]
pub async fn download_lsposed(
    window: tauri::Window,
    version: String,
    variant: String,
    url: String,
    asset_name: String,
    use_mirror: bool,
) -> Result<String> {
    use tauri::Emitter;

    let w = window.clone();
    let v = version.clone();
    let var = variant.clone();

    let result = crate::packages::download_lsposed(
        &version,
        &variant,
        &url,
        &asset_name,
        use_mirror,
        move |p| {
            let _ = w.emit("root-download-progress", &p);
        },
    )
    .await;

    let _ = window.emit(
        "root-download-progress",
        crate::roots::DownloadProgress {
            manager: "lsposed".into(),
            version: v,
            downloaded: 0,
            total: 0,
            percent: if result.is_ok() { 100.0 } else { 0.0 },
            status: if result.is_ok() { "done".into() } else { "error".into() },
            message: match &result {
                Ok(m) => m.clone(),
                Err(e) => e.to_string(),
            },
        },
    );
    let _ = var;

    result
}

#[tauri::command]
pub fn remove_lsposed(version: String, variant: String) -> Result<()> {
    crate::packages::remove_lsposed(&version, &variant)
}

// ------------------------------------------------------------------ 模块仓库

#[tauri::command]
pub async fn search_modules(
    org: String,
    query: String,
    page: u32,
) -> Result<Vec<crate::packages::ModuleInfo>> {
    crate::packages::search_modules(&org, &query, page).await
}

#[tauri::command]
pub async fn module_release(full_name: String, kind: String) -> Result<crate::packages::ModuleInfo> {
    crate::packages::module_release(&full_name, &kind).await
}

#[tauri::command]
pub async fn download_module(
    full_name: String,
    apk_url: String,
    apk_name: String,
    use_mirror: bool,
) -> Result<String> {
    crate::packages::download_module(&full_name, &apk_url, &apk_name, use_mirror).await
}

// ------------------------------------------------------------------ 版本搜索

#[tauri::command]
pub fn search_root_versions(manager: String, query: String) -> Vec<String> {
    let repo = match manager.as_str() {
        "magisk" => "topjohnwu/Magisk",
        "ksu" => "tiann/KernelSU",
        "apatch" => "bmax121/APatch",
        "sukisu" => "SukiSU-Ultra/SukiSU-Ultra",
        "ksu-next" => "KernelSU-Next/KernelSU-Next",
        _ => return vec![],
    };

    let cache = crate::ghcache::read_cache();
    let Some(entry) = cache.get(repo) else {
        return vec![];
    };
    let Some(body) = entry.get("body").and_then(|v| v.as_str()) else {
        return vec![];
    };

    #[derive(serde::Deserialize)]
    struct R {
        tag_name: String,
    }

    let Ok(rels) = serde_json::from_str::<Vec<R>>(body) else {
        return vec![];
    };

    let q = query.trim().trim_start_matches('v').to_lowercase();

    rels.into_iter()
        .map(|r| r.tag_name.trim_start_matches('v').to_string())
        .filter(|v| q.is_empty() || v.to_lowercase().contains(&q))
        .collect()
}

// ------------------------------------------------------------------ AOSP 镜像

#[tauri::command]
pub async fn list_aosp_images() -> Result<Vec<crate::aosp::ImageEntry>> {
    crate::aosp::list_images().await
}

#[tauri::command]
pub async fn download_aosp_image(
    window: tauri::Window,
    api: u32,
    url: String,
) -> Result<String> {
    use tauri::Emitter;

    let w = window.clone();
    let result = crate::aosp::download_image(api, &url, move |p| {
        let _ = w.emit("aosp-image-progress", &p);
    })
    .await;

    match &result {
        Ok(msg) => {
            let _ = window.emit(
                "aosp-image-progress",
                crate::aosp::ImageProgress {
                    api,
                    downloaded: 0,
                    total: 0,
                    percent: 100.0,
                    stage: "done".into(),
                    message: msg.clone(),
                },
            );
        }
        Err(e) => {
            let _ = window.emit(
                "aosp-image-progress",
                crate::aosp::ImageProgress {
                    api,
                    downloaded: 0,
                    total: 0,
                    percent: 0.0,
                    stage: "error".into(),
                    message: e.to_string(),
                },
            );
        }
    }

    result
}

#[tauri::command]
pub fn remove_aosp_image(api: u32) -> Result<()> {
    crate::aosp::remove_image(api)
}

// ------------------------------------------------------------------ 云端产物

#[tauri::command]
pub async fn fetch_artifact_manifest() -> Result<crate::artifacts::ArtifactManifest> {
    crate::artifacts::fetch_manifest().await
}

#[tauri::command]
pub async fn list_kernels() -> Result<Vec<crate::artifacts::KernelStatus>> {
    crate::artifacts::list_kernels().await
}

#[tauri::command]
pub async fn install_kernel(
    window: tauri::Window,
    id: String,
) -> Result<String> {
    use tauri::Emitter;

    let w = window.clone();
    let result = crate::artifacts::install_kernel(&id, move |p| {
        let _ = w.emit("kernel-install-progress", &p);
    })
    .await;

    let _ = window.emit(
        "kernel-install-progress",
        crate::artifacts::KernelProgress {
            id: id.clone(),
            downloaded: 0,
            total: 0,
            percent: if result.is_ok() { 100.0 } else { 0.0 },
            stage: if result.is_ok() { "done".into() } else { "error".into() },
            message: match &result {
                Ok(m) => m.clone(),
                Err(e) => e.to_string(),
            },
        },
    );

    result
}

#[tauri::command]
pub fn uninstall_kernel(id: String) -> Result<()> {
    crate::artifacts::uninstall_kernel(&id)
}

#[tauri::command]
pub fn restore_kernel_backup() -> Result<()> {
    crate::artifacts::restore_backup()
}

#[cfg(test)]
mod runtime_state_tests {
    use super::runtime_pending_restart;
    use crate::types::VmConfig;
    use serde_json::json;

    fn matched() -> VmConfig {
        serde_json::from_value(json!({
            "id": "runtime-state-fixture", "name": "pure state fixture",
            "backend": "android-emulator", "root_manager": "magisk@28.0",
            "root_applied": "magisk@28.0", "lsp_version": "none", "lsp_applied": "none"
        })).unwrap()
    }

    // Cloud-only regression execution. No paths, SDK, ADB, or processes are used.
    #[test]
    fn failed_root_verification_keeps_matching_selection_pending() {
        let running = matched();
        let mut desired = running.clone();
        desired.root_applied = None;
        assert!(runtime_pending_restart(&desired, &running, false, false));
        // A stale success string must not override a fresh failed observation.
        desired.root_applied = Some(desired.root_manager.clone());
        assert!(runtime_pending_restart(&desired, &running, false, false));
        assert!(!runtime_pending_restart(&desired, &running, false, true));
    }

    #[test]
    fn stock_selection_also_requires_confirmed_root_observation() {
        let mut running = matched();
        running.root_manager = "none".into();
        running.root_applied = Some("none".into());
        assert!(!runtime_pending_restart(&running, &running, false, true));
        assert!(runtime_pending_restart(&running, &running, false, false));
    }

    #[test]
    fn resource_version_and_module_reboot_differences_stay_pending() {
        let running = matched();
        assert!(runtime_pending_restart(&running, &running, true, true));
        for change in 0..8 {
            let mut desired = running.clone();
            match change {
                0 => desired.memory_mb += 256,
                1 => desired.cores += 1,
                2 => desired.root_manager = "magisk@29.0".into(),
                3 => desired.prepared_ramdisk = "different-fixture.img".into(),
                4 => desired.prepared_root = "magisk@29.0".into(),
                5 => desired.lsp_applied = None,
                6 => desired.root_applied = None,
                7 => desired.lsp_version = Some("1.9.2-zygisk".into()),
                _ => unreachable!(),
            }
            assert!(runtime_pending_restart(&desired, &running, false, true), "difference {change} was lost");
        }
    }
}
