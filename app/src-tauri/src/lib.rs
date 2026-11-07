//! Roost — Windows 原生 Android 容器管理器
//!
//! 后端全 Rust 实现：虚拟机管理、ADB 通信、Root 切换、崩溃诊断。

pub mod adb;
pub mod aosp;
pub mod artifacts;
mod background;
pub mod catalog;
pub mod commands;
pub mod compat;
pub mod diagnose;
pub mod packages;
pub mod ghcache;
pub mod guest;
pub mod headless;
pub mod proxy;
pub mod root;
pub mod roots;
pub mod types;
pub mod vm;
pub mod managed;
#[cfg(windows)]
mod owned_process;

pub use types::Error;

/// 生成 ISO 8601 时间戳
pub fn now_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // 简化版：Unix 秒转年月日
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;

    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;

    // 从 1970-01-01 开始推算日期
    let (year, month, day) = civil_from_days(days_since_epoch as i64);

    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Howard Hinnant 的 days-to-civil 算法
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

/// 应用入口
pub fn run() {
    let state = commands::AppState::new();

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            // 环境
            commands::get_env_status,
            commands::get_project_root,
            // 虚拟机
            commands::list_vms,
            commands::list_prepared_boots,
            commands::get_vm,
            commands::save_vm,
            commands::delete_vm,
            commands::create_vm,
            commands::launch_vm,
            commands::stop_vm,
            commands::vm_running,
            commands::running_vms,
            commands::vm_disk_info,
            commands::build_vm_command,
            // Overlay
            commands::list_overlays,
            commands::get_overlay,
            // ADB
            commands::adb_connect,
            commands::adb_device_info,
            commands::vm_boot_status,
            commands::request_vm_root,
            commands::adb_list_packages,
            commands::capture_vm_guest,
            commands::tap_vm_guest,
            commands::launch_vm_app,
            commands::adb_install,
            commands::install_magisk_module,
            commands::initialize_vm_magisk,
            commands::apply_vm_lsposed,
            commands::verify_vm_runtime,
            commands::adb_uninstall,
            commands::adb_logcat,
            commands::adb_crash_log,
            // Root
            commands::switch_root,
            commands::verify_root,
            commands::check_root,
            // 诊断
            commands::diagnose_app,
            // 兼容性矩阵
            commands::list_compat,
            commands::upsert_compat,
            commands::set_compat_state,
            commands::remove_compat,
            commands::compat_stats,
            commands::export_compat_csv,
            // Root 下载
            commands::list_root_managers,
            commands::download_root,
            commands::remove_root_version,
            commands::check_root_downloaded,
            // LSPosed
            commands::list_lsposed,
            commands::download_lsposed,
            commands::remove_lsposed,
            // 模块仓库
            commands::search_modules,
            commands::module_release,
            commands::download_module,
            // 版本搜索
            commands::search_root_versions,
            // AOSP 镜像
            commands::list_aosp_images,
            commands::download_aosp_image,
            commands::remove_aosp_image,
            // 云端产物
            commands::fetch_artifact_manifest,
            commands::list_kernels,
            commands::install_kernel,
            commands::uninstall_kernel,
            commands::restore_kernel_backup,
        ])
        .run(tauri::generate_context!());

    if let Err(e) = result {
        eprintln!("Roost 启动失败: {e}");
        std::process::exit(1);
    }
}
