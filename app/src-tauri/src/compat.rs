//! 兼容性矩阵
//!
//! 记录每个测试过的应用：能否运行、卡在哪、已用什么规避手段。

use crate::types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

// ------------------------------------------------------------------ 记录

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppRecord {
    pub package: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub abi: String,
    #[serde(default = "default_state")]
    pub state: String,
    #[serde(default = "default_root")]
    pub root_mode: String,
    #[serde(default)]
    pub issue: String,
    #[serde(default)]
    pub workaround: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub last_tested: String,
    #[serde(default)]
    pub test_count: u32,
}

fn default_state() -> String { "未测试".into() }
fn default_root() -> String { "none".into() }

impl AppRecord {
    pub fn new(package: impl Into<String>) -> Self {
        Self {
            package: package.into(),
            name: String::new(),
            version: String::new(),
            abi: String::new(),
            state: default_state(),
            root_mode: default_root(),
            issue: String::new(),
            workaround: String::new(),
            notes: String::new(),
            last_tested: String::new(),
            test_count: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MatrixFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub apps: Vec<AppRecord>,
}

fn default_version() -> u32 { 1 }

// ------------------------------------------------------------------ 矩阵

#[derive(Clone)]
pub struct Matrix {
    path: Arc<std::path::PathBuf>,
    records: Arc<parking_lot::Mutex<HashMap<String, AppRecord>>>,
}

impl Matrix {
    pub fn new() -> Self {
        let path = compat_dir().join("matrix.json");
        let records = Self::load_from(&path);
        Self {
            path: Arc::new(path),
            records: Arc::new(parking_lot::Mutex::new(records)),
        }
    }

    fn load_from(path: &std::path::Path) -> HashMap<String, AppRecord> {
        if !path.exists() {
            return HashMap::new();
        }

        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<MatrixFile>(&t).ok())
            .map(|f| {
                f.apps
                    .into_iter()
                    .map(|r| (r.package.clone(), r))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 保存到磁盘
    pub fn save(&self) -> Result<()> {
        let records = self.records.lock();

        let mut apps: Vec<AppRecord> = records.values().cloned().collect();
        apps.sort_by(|a, b| a.package.cmp(&b.package));

        let file = MatrixFile {
            version: 1,
            updated: crate::now_string(),
            apps,
        };

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(&file)?;
        std::fs::write(self.path.as_path(), json)?;
        Ok(())
    }

    /// 列出所有记录
    pub fn list(&self) -> Vec<AppRecord> {
        let records = self.records.lock();
        let mut apps: Vec<AppRecord> = records.values().cloned().collect();
        apps.sort_by(|a, b| a.package.cmp(&b.package));
        apps
    }

    /// 添加或更新
    pub fn upsert(&self, record: AppRecord) -> Result<()> {
        {
            let mut records = self.records.lock();
            let entry = records
                .entry(record.package.clone())
                .or_insert_with(|| AppRecord::new(&record.package));

            if !record.name.is_empty() {
                entry.name = record.name;
            }
            if !record.version.is_empty() {
                entry.version = record.version;
            }
            if !record.abi.is_empty() {
                entry.abi = record.abi;
            }
            if record.state != "未测试" || entry.state == "未测试" {
                entry.state = record.state;
            }
            if !record.root_mode.is_empty() {
                entry.root_mode = record.root_mode;
            }
            if !record.issue.is_empty() {
                entry.issue = record.issue;
            }
            if !record.workaround.is_empty() {
                entry.workaround = record.workaround;
            }
            if !record.notes.is_empty() {
                entry.notes = record.notes;
            }

            entry.last_tested = crate::now_string();
            entry.test_count += 1;
        }

        self.save()
    }

    /// 设置状态
    pub fn set_state(&self, package: &str, state: &str, issue: &str) -> Result<()> {
        {
            let mut records = self.records.lock();
            let entry = records
                .entry(package.to_string())
                .or_insert_with(|| AppRecord::new(package));
            entry.state = state.to_string();
            if !issue.is_empty() {
                entry.issue = issue.to_string();
            }
            entry.last_tested = crate::now_string();
            entry.test_count += 1;
        }

        self.save()
    }

    /// 删除
    pub fn remove(&self, package: &str) -> Result<bool> {
        let removed = {
            let mut records = self.records.lock();
            records.remove(package).is_some()
        };

        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// 统计
    pub fn stats(&self) -> HashMap<String, u32> {
        let records = self.records.lock();
        let mut stats: HashMap<String, u32> = HashMap::new();
        for r in records.values() {
            *stats.entry(r.state.clone()).or_insert(0) += 1;
        }
        stats
    }

    /// 导出 CSV
    pub fn export_csv(&self) -> Result<std::path::PathBuf> {
        let out = compat_dir().join("matrix.csv");
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let records = self.records.lock();
        let mut apps: Vec<&AppRecord> = records.values().collect();
        apps.sort_by(|a, b| a.package.cmp(&b.package));

        let mut csv = String::from(
            "package,name,version,abi,state,root_mode,issue,workaround,notes,last_tested,test_count\n",
        );

        for r in apps {
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{}\n",
                escape_csv(&r.package),
                escape_csv(&r.name),
                escape_csv(&r.version),
                escape_csv(&r.abi),
                escape_csv(&r.state),
                escape_csv(&r.root_mode),
                escape_csv(&r.issue),
                escape_csv(&r.workaround),
                escape_csv(&r.notes),
                escape_csv(&r.last_tested),
                r.test_count
            ));
        }

        // UTF-8 BOM 让 Excel 正确识别中文
        let bytes = [&[0xEF, 0xBB, 0xBF][..], csv.as_bytes()].concat();
        std::fs::write(&out, bytes)?;
        Ok(out)
    }
}

impl Default for Matrix {
    fn default() -> Self {
        Self::new()
    }
}

fn escape_csv(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
