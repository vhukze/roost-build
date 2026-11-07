//! 虚拟机进程管理
//!
//! 负责 QEMU 进程的启动、停止与状态跟踪。
//! 使用 tokio 异步管理，不阻塞 UI。

use crate::types::*;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::{Child, Command};

/// 单个运行中虚拟机的句柄
struct RunningVm {
    child: Child,
    config: VmConfig,
    #[cfg(windows)]
    job: crate::owned_process::ProcessJob,
}

impl RunningVm {
    async fn stop_owned_tree(&mut self) -> Result<()> {
        #[cfg(windows)]
        {
            if self.job.active_processes()? > 0 { self.job.terminate()?; }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            while self.job.active_processes()? > 0 {
                if std::time::Instant::now() >= deadline {
                    return Err(Error::Other("VM专属Job内仍有进程，保留ownership待重试".into()));
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
        if self.child.try_wait()?.is_none() {
            tokio::time::timeout(std::time::Duration::from_secs(15), self.child.kill()).await
                .map_err(|_| Error::Other("停止已持有的VM launcher超时".into()))??;
        }
        Ok(())
    }
}

/// Separate device and guest-interaction lanes share one exclusive lifecycle gate.
#[derive(Default)]
struct VmOperationLocks {
    lifecycle: Arc<tokio::sync::RwLock<()>>,
    device: Arc<tokio::sync::Mutex<()>>,
    interaction: Arc<tokio::sync::Mutex<()>>,
}

/// Keep the lane and lifecycle read lease for the caller's entire async operation.
/// Field order releases the lane before lifecycle, so a writer never races a lane.
pub struct VmOperationGuard {
    _lane: tokio::sync::OwnedMutexGuard<()>,
    _lifecycle: tokio::sync::OwnedRwLockReadGuard<()>,
}

/// 全局虚拟机管理器
#[derive(Clone)]
pub struct VmManager {
    inner: Arc<Mutex<HashMap<String, RunningVm>>>,
    operations: Arc<Mutex<HashMap<String, Arc<VmOperationLocks>>>>,
}

/// 启动结果
#[derive(Debug, Clone, serde::Serialize)]
pub struct LaunchResult {
    pub ok: bool,
    pub pid: Option<u32>,
    pub message: String,
    pub command: Vec<String>,
    pub backend: String,
    pub serial: String,
}

impl Default for VmManager {
    fn default() -> Self {
        Self::new()
    }
}

impl VmManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            operations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn operation_locks(&self, vm_id: &str) -> Result<Arc<VmOperationLocks>> {
        validate_component(vm_id)?;
        Ok(self.operations.lock().entry(vm_id.to_string())
            .or_insert_with(|| Arc::new(VmOperationLocks::default())).clone())
    }

    /// Ordinary device commands remain exclusive, but do not block screenshot/tap.
    pub fn operation(&self, vm_id: &str) -> Result<VmOperationGuard> {
        let locks = self.operation_locks(vm_id)?;
        let lifecycle = locks.lifecycle.clone().try_read_owned()
            .map_err(|_| Error::Other("目标VM正在切换生命周期，请稍后重试".into()))?;
        let lane = locks.device.clone().try_lock_owned()
            .map_err(|_| Error::Other("目标VM正在执行另一设备操作，请稍后重试".into()))?;
        Ok(VmOperationGuard { _lane: lane, _lifecycle: lifecycle })
    }

    /// Guest capture/tap serialize with each other while an explicit su may wait.
    pub fn interaction(&self, vm_id: &str) -> Result<VmOperationGuard> {
        let locks = self.operation_locks(vm_id)?;
        let lifecycle = locks.lifecycle.clone().try_read_owned()
            .map_err(|_| Error::Other("目标VM正在切换生命周期，请稍后重试".into()))?;
        let lane = locks.interaction.clone().try_lock_owned()
            .map_err(|_| Error::Other("目标VM正在采集或点击Guest画面，请稍后重试".into()))?;
        Ok(VmOperationGuard { _lane: lane, _lifecycle: lifecycle })
    }

    /// Fail fast until both lanes release their read leases; never queue a writer.
    pub fn lifecycle_operation(&self, vm_id: &str) -> Result<tokio::sync::OwnedRwLockWriteGuard<()>> {
        self.operation_locks(vm_id)?.lifecycle.clone().try_write_owned()
            .map_err(|_| Error::Other("目标VM有进行中的操作或生命周期切换，请稍后重试".into()))
    }

    pub fn adb_for_vm(&self, vm_id: &str) -> Result<crate::adb::Adb> {
        if !self.is_running(vm_id) { return Err(Error::Other("目标 VM 未运行".into())); }
        let map = self.inner.lock();
        let vm = map.get(vm_id).ok_or_else(|| Error::Other("VM 已退出".into()))?;
        crate::adb::Adb::for_vm(&vm.config)
    }

    pub fn running_config(&self, vm_id: &str) -> Result<VmConfig> {
        self.inner.lock().get(vm_id).map(|v| v.config.clone())
            .ok_or_else(|| Error::Other("VM 未运行".into()))
    }

    /// Launch guard prevents duplicate processes, including during preparation.
    pub async fn launch(&self, vm_id: &str, headless: bool) -> Result<LaunchResult> {
        validate_component(vm_id)?;
        let _operation = self.lifecycle_operation(vm_id)?;
        self.launch_inner(VmConfig::load(vm_id)?, headless).await
    }

    /// Use the caller's validated snapshot without reloading the target serial.
    pub(crate) async fn launch_captured(&self, config: VmConfig, headless: bool) -> Result<LaunchResult> {
        config.validate()?;
        let _operation = self.lifecycle_operation(&config.id)?;
        self.launch_inner(config, headless).await
    }

    async fn launch_inner(&self, config: VmConfig, headless: bool) -> Result<LaunchResult> {
        let vm_id = config.id.as_str();
        // Refresh stale child state before checking duplicate launch.
        self.is_running(vm_id);
        {
            let map = self.inner.lock();
            if map.contains_key(vm_id) {
                return Ok(LaunchResult {
                    ok: false,
                    pid: None,
                    message: "虚拟机已在运行".into(),
                    command: vec![],
                    backend: String::new(), serial: String::new(),
                });
            }
        }

        config.validate()?;
        if config.backend == "legacy-qemu" {
            std::net::TcpListener::bind(("127.0.0.1", config.adb_port))
                .map_err(|_| Error::Other("ADB转发端口已被占用，未连接既有设备".into()))?;
        }
        if config.backend == "android-emulator" {
            for port in [config.console_port, config.console_port + 1] {
                std::net::TcpListener::bind(("127.0.0.1", port))
                    .map_err(|_| Error::Other(format!("端口 {port} 已占用；不连接或停止既有设备")))?;
            }
        }
        let serial = config.serial()?;
        // Serialize port ownership checks across concurrent launches.
        let used_ports = |c: &VmConfig| if c.backend == "android-emulator" { vec![c.console_port, c.console_port + 1] } else { vec![c.adb_port] };
        for other in VmConfig::list().iter().filter(|v| v.id != config.id) {
            if used_ports(other).iter().any(|port| used_ports(&config).contains(port)) {
                return Err(Error::Other("设备 serial/端口已被其他 VM 配置占用".into()));
            }
        }

        // Legacy QEMU checks are not prerequisites for Android Emulator.
        let env = EnvStatus::check();
        if config.backend == "legacy-qemu" {
        if let Err(e) = env.require_qemu() {
            return Ok(LaunchResult {
                ok: false,
                pid: None,
                message: e.to_string(),
                command: vec![],
                backend: config.backend.clone(), serial: serial.clone(),
            });
        }
        if let Err(e) = env.require_images() {
            return Ok(LaunchResult {
                ok: false,
                pid: None,
                message: e.to_string(),
                command: vec![],
                backend: config.backend.clone(), serial: serial.clone(),
            });
        }

            self.ensure_disk(&config).await?;
        }
        crate::managed::prepare_boot(&config)?;
        let args = if config.backend == "android-emulator" {
            crate::managed::check_acceleration(&config).await?;
            crate::managed::prepare_emulator_data(&config)?;
            crate::managed::emulator_command(&config, headless)?
        } else { config.build_command(headless) };
        let exe = &args[0];
        let rest = &args[1..];

        let mut cmd = Command::new(exe);
        if config.backend == "android-emulator" {
            cmd.env("ANDROID_AVD_HOME", config.dir().join("avd"));
            cmd.env("ANDROID_EMULATOR_HOME", config.dir().join("emulator-home"));
            cmd.env("ANDROID_SDK_ROOT", &config.sdk_path);
            cmd.env("ANDROID_HOME", &config.sdk_path);
            std::fs::create_dir_all(config.dir().join("emulator-home"))?;
        }
        let log_dir = config.dir().join("logs");
        std::fs::create_dir_all(&log_dir)?;
        let launch_log = std::fs::OpenOptions::new().create(true).append(true).open(log_dir.join("launch.log"))?;
        let error_log = launch_log.try_clone()?;
        cmd.args(rest).stdin(Stdio::null()).stdout(Stdio::from(launch_log)).stderr(Stdio::from(error_log));

        #[cfg(windows)]
        let job = crate::owned_process::ProcessJob::new()?;
        #[cfg(windows)]
        {
            const CREATE_SUSPENDED: u32 = 0x0000_0004;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            // Console visibility is independent of Android's GUI/headless mode.
            cmd.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
        }

        match cmd.spawn() {
            Ok(mut child) => {
                #[cfg(windows)]
                if let Err(error) = job.assign(&child).and_then(|_| crate::owned_process::resume_initial_thread(&child)) {
                    let _ = job.terminate();
                    let cleanup = tokio::time::timeout(std::time::Duration::from_secs(15), child.kill()).await;
                    let cleanup_note = match cleanup { Ok(Ok(())) => String::new(), other => format!("；新进程清理结果: {other:?}") };
                    if child.try_wait().ok().flatten().is_none() || job.active_processes().map(|count| count > 0).unwrap_or(true) {
                        self.inner.lock().insert(vm_id.to_string(), RunningVm { child, config: config.clone(), job });
                    }
                    return Err(Error::Other(format!("建立VM进程树所有权失败: {error}{cleanup_note}")));
                }
                let pid = child.id();
                // Persist only after process ownership is recorded; save failure must not orphan a Child.
                let mut persisted = config.clone();
                persisted.pending_restart = true;
                self.inner.lock().insert(
                    vm_id.to_string(),
                    RunningVm {
                        child,
                        config: config.clone(),
                        #[cfg(windows)]
                        job,
                    },
                );

                let save_note = persisted.save().err().map(|e| format!("；状态保存失败: {e}")).unwrap_or_default();
                Ok(LaunchResult {
                    ok: true,
                    pid,
                    message: format!("{} 已启动；Root/LSPosed 等待验证{save_note}", config.backend),
                    command: args,
                backend: config.backend.clone(), serial: serial.clone(),
                })
            }
            Err(e) => Ok(LaunchResult {
                ok: false,
                pid: None,
                message: format!("{} 启动失败: {e}", config.backend),
                command: args,
                backend: config.backend.clone(), serial: serial.clone(),
            }),
        }
    }

    /// 停止虚拟机
    pub async fn stop(&self, vm_id: &str) -> Result<String> {
        let _operation = self.lifecycle_operation(vm_id)?;
        let mut running = {
            let mut map = self.inner.lock();
            map.remove(vm_id)
        };

        match running.take() {
            Some(mut vm) => {
                let result: Result<()> = async {
                    if vm.config.backend == "android-emulator" {
                        // Windows uses only the retained Job handle. A serial/port
                        // is not ownership: another emulator may reuse it after exit.
                        #[cfg(not(windows))]
                        {
                            let adb = crate::adb::Adb::for_vm(&vm.config)?;
                            let graceful = adb.emulator_kill().await.map(|result| result.ok).unwrap_or(false);
                            if graceful {
                                let _ = tokio::time::timeout(std::time::Duration::from_secs(25), vm.child.wait()).await;
                            }
                        }
                        // The retained Job includes descendants after launcher exit;
                        // no process is selected for termination by name or serial.
                        vm.stop_owned_tree().await?;
                        // A reused port does not revive a completed Windows Job.
                        #[cfg(not(windows))]
                        {
                        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
                        loop {
                            let free = [vm.config.console_port, vm.config.console_port + 1].iter()
                                .all(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok());
                            if free { break; }
                            if std::time::Instant::now() >= deadline { return Err(Error::Other("Emulator端口未释放，保留ownership待重试".into())); }
                            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        }
                        }
                    } else {
                        vm.stop_owned_tree().await?;
                    }
                    Ok(())
                }.await;
                if let Err(error) = result {
                    self.inner.lock().insert(vm_id.to_string(), vm);
                    return Err(error);
                }
                Ok("该VM本会话持有的进程树已停止；未接管同端口的外部实例".into())
            }
            None => Ok("虚拟机未运行".into()),
        }
    }

    /// 查询运行状态
    pub fn is_running(&self, vm_id: &str) -> bool {
        let mut map = self.inner.lock();
        if let Some(vm) = map.get_mut(vm_id) {
            // try_wait 不阻塞，返回 Some 说明已退出
            match vm.child.try_wait() {
                Ok(Some(_)) => {
                    #[cfg(windows)]
                    if vm.job.active_processes().map(|count| count > 0).unwrap_or(true) { return true; }
                    #[cfg(not(windows))]
                    if vm.config.backend == "android-emulator" && [vm.config.console_port, vm.config.console_port + 1].iter()
                        .any(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_err()) { return true; }
                    // On Windows an empty retained Job is definitive. Reused
                    // ports belong to a different launch, not to this manager.
                    map.remove(vm_id);
                    false
                }
                Ok(None) => true,
                Err(_) => true, // retain ownership until an explicit stop confirms exit
            }
        } else {
            false
        }
    }

    /// 列出所有运行中的虚拟机
    pub fn running_ids(&self) -> Vec<String> {
        let ids: Vec<String> = self.inner.lock().keys().cloned().collect();
        ids.into_iter().filter(|id| self.is_running(id)).collect()
    }

    /// 确保用户盘存在，不存在则创建
    async fn ensure_disk(&self, config: &VmConfig) -> Result<()> {
        let disk = config.disk_path();
        if disk.exists() {
            return Ok(());
        }

        std::fs::create_dir_all(config.dir())?;

        let output = crate::background::async_command(qemu_img_exe())
            .args([
                "create",
                "-f",
                "qcow2",
                disk.to_string_lossy().as_ref(),
                "16G",
            ])
            .output()
            .await?;

        if !output.status.success() {
            return Err(Error::Command(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod operation_lane_tests {
    use super::VmManager;

    // Lock maps only: no configs, process handles, ADB, files or Tokio runtime.
    #[test]
    fn ordinary_and_interaction_lanes_coexist_but_each_lane_is_exclusive() {
        let manager = VmManager::new();
        let device = manager.operation("fixture").unwrap();
        let interaction = manager.interaction("fixture").unwrap();
        assert!(manager.operation("fixture").is_err());
        assert!(manager.interaction("fixture").is_err());
        assert!(manager.lifecycle_operation("fixture").is_err());
        drop(device);
        assert!(manager.lifecycle_operation("fixture").is_err());
        drop(interaction);
        assert!(manager.lifecycle_operation("fixture").is_ok());
    }

    #[test]
    fn lifecycle_gate_excludes_both_lanes_and_other_writers() {
        let manager = VmManager::new();
        let lifecycle = manager.lifecycle_operation("fixture").unwrap();
        assert!(manager.operation("fixture").is_err());
        assert!(manager.interaction("fixture").is_err());
        assert!(manager.lifecycle_operation("fixture").is_err());
        drop(lifecycle);
        let interaction = manager.interaction("fixture").unwrap();
        let device = manager.operation("fixture").unwrap();
        drop(interaction);
        assert!(manager.lifecycle_operation("fixture").is_err());
        drop(device);
        assert!(manager.lifecycle_operation("fixture").is_ok());
    }

    #[test]
    fn clones_share_lanes_but_vm_ids_and_manager_instances_are_independent() {
        let manager = VmManager::new();
        let clone = manager.clone();
        let other = VmManager::new();
        let _device = manager.operation("fixture").unwrap();
        assert!(clone.operation("fixture").is_err());
        let _interaction = clone.interaction("fixture").unwrap();
        assert!(manager.interaction("fixture").is_err());
        assert!(clone.lifecycle_operation("fixture").is_err());
        assert!(manager.lifecycle_operation("different-vm").is_ok());
        assert!(other.lifecycle_operation("fixture").is_ok());
    }

    #[test]
    fn all_lanes_reject_invalid_vm_identity_before_creating_lock_entries() {
        let manager = VmManager::new();
        for id in ["", ".", "..", "../other", "vm/other", "vm\\other", "vm id", "vm;id", "vm\n"] {
            assert!(manager.operation(id).is_err(), "{id:?}");
            assert!(manager.interaction(id).is_err(), "{id:?}");
            assert!(manager.lifecycle_operation(id).is_err(), "{id:?}");
        }
        assert!(manager.operations.lock().is_empty());
    }
}

/// 获取虚拟机磁盘信息
pub fn disk_info(vm_id: &str) -> Option<u64> {
    let config = VmConfig::load(vm_id).ok()?;
    let disk = if config.backend == "android-emulator" { config.dir().join("emulator/userdata-qemu.img") } else { config.disk_path() };
    if disk.exists() {
        std::fs::metadata(&disk).ok().map(|m| m.len())
    } else {
        None
    }
}
