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

/// 全局虚拟机管理器
#[derive(Clone)]
pub struct VmManager {
    inner: Arc<Mutex<HashMap<String, RunningVm>>>,
    operations: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
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

    /// All device mutations and lifecycle transitions share an instance-scoped lease.
    pub fn operation(&self, vm_id: &str) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        validate_component(vm_id)?;
        let mutex = self.operations.lock().entry(vm_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone();
        mutex.try_lock_owned().map_err(|_| Error::Other("目标VM正在执行另一操作，请稍后重试".into()))
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
        let _operation = self.operation(vm_id)?;
        self.launch_inner(vm_id, headless).await
    }

    async fn launch_inner(&self, vm_id: &str, headless: bool) -> Result<LaunchResult> {
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

        let config = VmConfig::load(vm_id)?;

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
            cmd.creation_flags(CREATE_SUSPENDED | if !headless { CREATE_NO_WINDOW } else { 0 });
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
        let _operation = self.operation(vm_id)?;
        let mut running = {
            let mut map = self.inner.lock();
            map.remove(vm_id)
        };

        match running.take() {
            Some(mut vm) => {
                let result: Result<()> = async {
                    if vm.config.backend == "android-emulator" {
                        let adb = crate::adb::Adb::for_vm(&vm.config)?;
                        let graceful = adb.emulator_kill().await.map(|result| result.ok).unwrap_or(false);
                        if graceful {
                            let _ = tokio::time::timeout(std::time::Duration::from_secs(25), vm.child.wait()).await;
                        }
                        // ADB can be absent before guest registration or during a hang.
                        // The retained Job handle identifies only this launch's tree,
                        // including descendants after the launcher exits.
                        vm.stop_owned_tree().await?;
                        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
                        loop {
                            let free = [vm.config.console_port, vm.config.console_port + 1].iter()
                                .all(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok());
                            if free { break; }
                            if std::time::Instant::now() >= deadline { return Err(Error::Other("Emulator端口未释放，保留ownership待重试".into())); }
                            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
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
                Ok("已停止该VM并确认Emulator端口释放".into())
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
                    if vm.config.backend == "android-emulator" && [vm.config.console_port, vm.config.console_port + 1].iter()
                        .any(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_err()) { return true; }
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

        let output = Command::new(qemu_img_exe())
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
