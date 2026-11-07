//! Root 切换引擎
//!
//! 核心设计：切换前用干净 base 覆盖回滚，而非运行卸载脚本。
//! 这样任意次数切换都不会残留污染。

use crate::adb::Adb;
use crate::types::*;
use std::path::Path;

/// 切换结果
#[derive(Debug, Clone, serde::Serialize)]
pub struct SwitchResult {
    pub ok: bool,
    pub from: String,
    pub to: String,
    pub message: String,
    pub rolled_back: bool,
    pub steps: Vec<String>,
}

/// 验证结果
#[derive(Debug, Clone, serde::Serialize)]
pub struct VerifyResult {
    pub ok: bool,
    pub message: String,
}

/// Root 引擎
pub struct RootEngine {
    adb: Adb,
}

impl RootEngine {
    pub fn new(adb: Adb) -> Self {
        Self { adb }
    }

    /// 读取 overlay 的清理清单
    fn remove_list(overlay_id: &str) -> Vec<String> {
        let path = overlays_dir().join(overlay_id).join("clean").join("remove.txt");
        if !path.exists() {
            return Vec::new();
        }

        std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.to_string())
            .collect()
    }

    /// 清理上一个 overlay
    async fn clean_previous(&self, previous: &str, steps: &mut Vec<String>) {
        let list = if previous.is_empty() || previous == "none" {
            Self::remove_list("none")
        } else {
            Self::remove_list(previous)
        };

        for path in &list {
            let _ = self.adb.shell(&format!("rm -rf {path}")).await;
        }

        steps.push(format!("清理 {} 个路径", list.len()));
    }

    /// 推送 overlay 的 files/ 内容
    async fn apply_files(&self, overlay_id: &str, steps: &mut Vec<String>) {
        let files_dir = overlays_dir().join(overlay_id).join("files");
        if !files_dir.exists() {
            steps.push("files/ 不存在，跳过".into());
            return;
        }

        let root_map = [
            ("system", "/system"),
            ("vendor", "/vendor"),
            ("product", "/product"),
            ("system_ext", "/system_ext"),
            ("odm", "/odm"),
            ("data", "/data"),
            ("sbin", "/sbin"),
        ];

        let mut pushed = 0usize;

        for (local_root, remote_root) in root_map {
            let dir = files_dir.join(local_root);
            if !dir.exists() {
                continue;
            }

            for entry in walkdir::WalkDir::new(&dir).into_iter().flatten() {
                if !entry.file_type().is_file() {
                    continue;
                }

                let rel = entry.path().strip_prefix(&dir).unwrap_or(Path::new(""));
                let remote = format!("{}/{}", remote_root, rel.to_string_lossy().replace('\\', "/"));

                // 确保父目录存在
                if let Some(parent) = Path::new(&remote).parent() {
                    let _ = self.adb.shell(&format!("mkdir -p {}", parent.to_string_lossy())).await;
                }

                if let Ok(r) = self.adb.push(entry.path().to_string_lossy().as_ref(), &remote).await {
                    if r.ok {
                        pushed += 1;
                    }
                }
            }
        }

        steps.push(format!("推送 {pushed} 个文件"));
    }

    /// 创建符号链接
    async fn apply_symlinks(&self, manifest: &OverlayManifest, steps: &mut Vec<String>) {
        if manifest.symlinks.is_empty() {
            return;
        }

        for link in &manifest.symlinks {
            let _ = self
                .adb
                .shell(&format!("ln -sf {} {}", link.target, link.path))
                .await;
        }

        steps.push(format!("创建 {} 个符号链接", manifest.symlinks.len()));
    }

    /// 应用 SELinux 规则
    async fn apply_sepolicy(&self, overlay_id: &str, steps: &mut Vec<String>) {
        let sep_dir = overlays_dir().join(overlay_id).join("sepolicy");
        if !sep_dir.exists() {
            return;
        }

        let te_files: Vec<_> = std::fs::read_dir(&sep_dir)
            .map(|d| {
                d.flatten()
                    .filter(|e| {
                        e.path().extension().map(|x| x == "te").unwrap_or(false)
                    })
                    .collect()
            })
            .unwrap_or_default();

        if te_files.is_empty() {
            return;
        }

        let remote_dir = "/data/local/tmp/roost-sepolicy";
        let _ = self.adb.shell(&format!("rm -rf {remote_dir}")).await;
        let _ = self.adb.shell(&format!("mkdir -p {remote_dir}")).await;

        for te in &te_files {
            let name = te.file_name().to_string_lossy().to_string();
            let _ = self
                .adb
                .push(
                    te.path().to_string_lossy().as_ref(),
                    &format!("{remote_dir}/{name}"),
                )
                .await;
        }

        // 尝试 magiskpolicy
        if let Ok(r) = self.adb.shell("which magiskpolicy 2>/dev/null").await {
            if !r.stdout.trim().is_empty() {
                let _ = self
                    .adb
                    .shell(&format!("magiskpolicy --live --apply {remote_dir}/*.te"))
                    .await;
                steps.push("用 magiskpolicy 应用 sepolicy".into());
                return;
            }
        }

        steps.push("未找到 sepolicy 工具，跳过".into());
    }

    /// 执行生命周期钩子
    async fn run_hook(&self, overlay_id: &str, hook: &str, steps: &mut Vec<String>) {
        let path = overlays_dir()
            .join(overlay_id)
            .join("hooks")
            .join(format!("{hook}.sh"));

        if !path.exists() {
            return;
        }

        let remote = format!("/data/local/tmp/roost-{hook}.sh");

        if self
            .adb
            .push(path.to_string_lossy().as_ref(), &remote)
            .await
            .map(|r| r.ok)
            .unwrap_or(false)
        {
            let _ = self.adb.shell(&format!("chmod 755 {remote}")).await;
            let _ = self.adb.shell(&format!("sh {remote}")).await;
            steps.push(format!("{hook} 执行完成"));
        }
    }

    /// 验证 Root 是否生效
    pub async fn verify(&self, overlay_id: &str) -> VerifyResult {
        let manifest = match OverlayManifest::load(&overlays_dir().join(overlay_id)) {
            Ok(m) => m,
            Err(e) => {
                return VerifyResult {
                    ok: false,
                    message: e.to_string(),
                }
            }
        };

        if overlay_id == "none" {
            let has_root = self.adb.check_root().await;
            return VerifyResult {
                ok: !has_root,
                message: if has_root { "系统仍有 root".into() } else { "已确认无 root".into() },
            };
        }

        // 检查 su 路径
        if let Some(su_path) = &manifest.checks.su_path {
            if let Ok(r) = self.adb.shell(&format!("[ -f {su_path} ] && echo exists")).await {
                if !r.stdout.contains("exists") {
                    return VerifyResult {
                        ok: false,
                        message: format!("su 路径不存在: {su_path}"),
                    };
                }
            }
        }

        // 验证命令
        if let Some(cmd) = &manifest.checks.verify_cmd {
            if let Ok(r) = self.adb.shell(cmd).await {
                if !r.ok {
                    return VerifyResult {
                        ok: false,
                        message: format!("验证命令失败: {cmd}"),
                    };
                }
            }
        }

        // 最终验证
        if self.adb.check_root().await {
            VerifyResult {
                ok: true,
                message: "root 生效".into(),
            }
        } else {
            VerifyResult {
                ok: false,
                message: "无法获取 root".into(),
            }
        }
    }

    /// 切换 Root
    pub async fn switch(&self, target: &str, previous: &str) -> Result<SwitchResult> {
        let manifest = OverlayManifest::load(&overlays_dir().join(target))?;
        let mut steps = Vec::new();

        // 1. 停 zygote
        let _ = self.adb.shell("stop").await;
        steps.push("停止 zygote".into());

        // 2. 系统挂可写
        let _ = self.adb.shell("mount -o remount,rw /system").await;
        steps.push("/system 挂载可写".into());

        // 3. 清理旧 overlay
        self.clean_previous(previous, &mut steps).await;

        // 4. pre_apply
        self.run_hook(target, "pre_apply", &mut steps).await;

        // 5. 推送文件
        self.apply_files(target, &mut steps).await;

        // 6. 符号链接
        self.apply_symlinks(&manifest, &mut steps).await;

        // 7. sepolicy
        self.apply_sepolicy(target, &mut steps).await;

        // 8. post_apply
        self.run_hook(target, "post_apply", &mut steps).await;

        // 9. 启动 zygote
        let _ = self.adb.shell("start").await;
        steps.push("启动 zygote".into());

        // 10. 等待并验证
        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        let verify = self.verify(target).await;

        if verify.ok {
            return Ok(SwitchResult {
                ok: true,
                from: previous.to_string(),
                to: target.to_string(),
                message: format!("切换成功: {previous} → {target}"),
                rolled_back: false,
                steps,
            });
        }

        // 11. 回滚
        steps.push(format!("验证失败（{}），开始回滚", verify.message));
        let _ = self.adb.shell("stop").await;
        self.clean_previous(target, &mut steps).await;
        self.apply_files(previous, &mut steps).await;
        let _ = self.adb.shell("start").await;
        steps.push(format!("已回滚到 {previous}"));

        Ok(SwitchResult {
            ok: false,
            from: previous.to_string(),
            to: target.to_string(),
            message: format!("切换失败: {}，已回滚", verify.message),
            rolled_back: true,
            steps,
        })
    }
}
