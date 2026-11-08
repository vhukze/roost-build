//! Managed Emulator boot and Magisk module operations. No host driver changes.
use crate::adb::{Adb, CmdResult};
use crate::types::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

pub fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop { let count = file.read(&mut buffer)?; if count == 0 { break; } hash.update(&buffer[..count]); }
    Ok(format!("{:x}", hash.finalize()))
}

fn commit_file(temporary: &Path, destination: &Path) -> Result<()> {
    let previous = destination.with_extension("previous");
    if destination.exists() {
        if previous.exists() { std::fs::remove_file(&previous)?; }
        std::fs::rename(destination, &previous)?;
    }
    if let Err(error) = std::fs::rename(temporary, destination) {
        if previous.exists() { let _ = std::fs::rename(&previous, destination); }
        return Err(error.into());
    }
    Ok(())
}

pub fn emulator_exe(c: &VmConfig) -> PathBuf { PathBuf::from(&c.sdk_path).join("emulator/emulator.exe") }

#[derive(Debug, Clone, Serialize)]
pub struct PreparedBoot {
    pub root: String,
    pub key: String,
    pub source: String,
    pub sha256: String,
    pub ramdisk: String,
}

fn image_identity(c: &VmConfig) -> Result<serde_json::Value> {
    let image = if c.backend == "android-emulator" { PathBuf::from(&c.emulator_image_dir) } else { base_images_dir() };
    let canonical = image.canonicalize()?;
    let prop = image.join("build.prop");
    Ok(serde_json::json!({ "backend": c.backend, "image": canonical.to_string_lossy(),
        "original_ramdisk_sha256": sha256_file(&image.join("ramdisk.img"))?,
        "build_prop_sha256": if prop.is_file() { Some(sha256_file(&prop)?) } else { None } }))
}

pub fn list_prepared_boots(c: &VmConfig) -> Result<Vec<PreparedBoot>> {
    list_prepared_boots_in_dir(c, &c.dir())
}

fn list_prepared_boots_in_dir(c: &VmConfig, vm_dir: &Path) -> Result<Vec<PreparedBoot>> {
    let boot = vm_dir.join("boot");
    if !boot.is_dir() { return Ok(vec![]); }
    let identity = image_identity(c)?;
    let canonical_boot = boot.canonicalize()?;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&boot)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }
        let key = entry.file_name().to_string_lossy().to_string();
        if validate_component(&key).is_err() { continue; }
        let dir = entry.path();
        let manifest = dir.join("prepared.json");
        let disk = dir.join("ramdisk.img");
        if !manifest.is_file() || !disk.is_file() { continue; }
        if !disk.canonicalize()?.starts_with(&canonical_boot) || !manifest.canonicalize()?.starts_with(&canonical_boot) { continue; }
        let parsed: serde_json::Value = match std::fs::read(&manifest).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
            Some(value) => value, None => continue,
        };
        let root = parsed["root"].as_str().unwrap_or("");
        if !root.starts_with("magisk@") || validate_component(root).is_err() || key != root.replace('@', "-") { continue; }
        if let Some(recorded_key) = parsed.get("key") {
            if recorded_key.as_str() != Some(key.as_str()) { continue; }
        }
        // Current records bind image fingerprints. Legacy records bind the explicit image directory
        // plus the retained ramdisk hash; a successful reuse upgrades the record format.
        if let Some(recorded) = parsed.get("image_identity") {
            if recorded != &identity { continue; }
        } else {
            let recorded = parsed["image_dir"].as_str().unwrap_or("");
            if c.backend != "android-emulator" || recorded.is_empty()
                || Path::new(recorded).canonicalize().ok() != Path::new(&c.emulator_image_dir).canonicalize().ok() { continue; }
        }
        let sha256 = parsed["sha256"].as_str().unwrap_or("");
        if sha256.len() != 64 || disk.metadata()?.len() == 0 || sha256_file(&disk)? != sha256 { continue; }
        entries.push(PreparedBoot { root: root.into(), key, source: parsed["source"].as_str().unwrap_or("").into(),
            sha256: sha256.into(), ramdisk: disk.to_string_lossy().into_owned() });
    }
    entries.sort_by(|left, right| left.root.cmp(&right.root));
    Ok(entries)
}

pub fn prepare_boot(c: &VmConfig) -> Result<()> {
    prepare_boot_in_dir(c, &c.dir())
}

fn prepare_boot_in_dir(c: &VmConfig, vm_dir: &Path) -> Result<()> {
    c.validate()?;
    let identity = image_identity(c)?;
    let cached = list_prepared_boots_in_dir(c, vm_dir)?.into_iter().find(|entry| entry.root == c.root_manager);
    let candidate = Path::new(&c.prepared_ramdisk);
    let inside_boot = candidate.canonicalize().ok().zip(vm_dir.join("boot").canonicalize().ok())
        .map(|(path, boot)| path.starts_with(boot)).unwrap_or(false);
    // A private cache path is never reclassified as an unchecked external prepared file.
    let supplied = c.prepared_root == c.root_manager && !c.prepared_ramdisk.trim().is_empty()
        && candidate.is_file() && !inside_boot;
    let source = if c.root_manager == "none" {
        if c.backend == "android-emulator" { PathBuf::from(&c.emulator_image_dir).join("ramdisk.img") }
        else { base_images_dir().join("ramdisk.img") }
    } else if supplied { PathBuf::from(&c.prepared_ramdisk) }
    else if let Some(entry) = &cached { PathBuf::from(&entry.ramdisk) }
    else { return Err(Error::Other(format!("{} 尚未准备匹配当前VM/镜像的ramdisk；配置prepared_root和prepared_ramdisk后重试", c.root_manager))); };
    if !source.is_file() || source.metadata()?.len() == 0 { return Err(Error::PathNotFound(source)); }
    let root_key = c.root_manager.replace('@', "-");
    let version_dir = vm_dir.join("boot").join(&root_key);
    std::fs::create_dir_all(&version_dir)?;
    let version_disk = version_dir.join("ramdisk.img");
    let temporary = version_dir.join("ramdisk.img.new");
    std::fs::copy(&source, &temporary)?;
    let source_hash = sha256_file(&source)?;
    if sha256_file(&temporary)? != source_hash { return Err(Error::Other("ramdisk复制校验失败".into())); }
    commit_file(&temporary, &version_disk)?;
    let launch_new = vm_dir.join("boot/ramdisk.img.new");
    std::fs::copy(&version_disk, &launch_new)?;
    commit_file(&launch_new, &vm_dir.join("boot/ramdisk.img"))?;
    // Keep original supplied provenance when reusing the VM-private copy.
    let provenance = if supplied || c.root_manager == "none" { source.to_string_lossy().into_owned() }
        else { cached.map(|entry| entry.source).unwrap_or_default() };
    std::fs::write(version_dir.join("prepared.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "root": c.root_manager, "key": root_key, "source": provenance,
        "bytes": version_disk.metadata()?.len(), "prepared_at": crate::now_string(),
        "sha256": source_hash, "image_dir": c.emulator_image_dir, "image_identity": identity,
        "requirements": "official cpio backup, KEEPVERITY/KEEPFORCEENCRYPT, PREINITDEVICE",
        "verification": "user-prepared; runtime verification pending"
    }))?)?;
    Ok(())
}

#[cfg(test)]
mod prepared_boot_tests {
    use super::{image_identity, list_prepared_boots_in_dir, prepare_boot_in_dir, sha256_file};
    use crate::types::VmConfig;
    use serde_json::{json, Value};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    // Cloud-only execution pending: GitHub Actions cargo test runs these fixtures.
    // Locally, parse this source only with rustfmt --emit stdout; no cargo/rustc.
    // Every path is below a unique temporary directory. Do not call c.dir(),
    // project_root(), environment setters, SDK discovery, ADB, or process launch.
    // No Drop cleanup: fixture directories may remain on the disposable CI runner.
    const ROOT_A: &str = "magisk@28.0";
    const ROOT_B: &str = "magisk@29.0";
    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        base: PathBuf,
        image: PathBuf,
        vm_dir: PathBuf,
        config: VmConfig,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let base = std::env::temp_dir().join(format!(
                "droidbox-prepared-{label}-{}-{nonce}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            // create_dir refuses an existing fixture rather than overwriting it.
            fs::create_dir(&base).unwrap();
            let image = base.join("fake-image");
            let vm_dir = base.join("vm-a");
            fs::create_dir(&image).unwrap();
            fs::create_dir(&vm_dir).unwrap();
            fs::write(image.join("ramdisk.img"), b"original ramdisk fixture").unwrap();
            fs::write(image.join("build.prop"), b"ro.build.fingerprint=fixture/original\n").unwrap();
            let config: VmConfig = serde_json::from_value(json!({
                "id": "vm-a", "name": "isolated prepared boot test",
                "backend": "android-emulator", "root_manager": ROOT_A,
                "emulator_image_dir": image.to_string_lossy(),
                "sdk_path": base.join("unused-sdk").to_string_lossy(),
                "prepared_root": "",
                "prepared_ramdisk": base.join("absent-prepared.img").to_string_lossy()
            })).unwrap();
            config.validate().unwrap();
            Self { base, image, vm_dir, config }
        }

        fn record(&self, vm_dir: &Path, directory_key: &str, root: &str, bytes: &[u8]) -> Value {
            let dir = vm_dir.join("boot").join(directory_key);
            fs::create_dir_all(&dir).unwrap();
            let disk = dir.join("ramdisk.img");
            fs::write(&disk, bytes).unwrap();
            json!({
                "root": root, "key": directory_key,
                // Provenance is deliberately absent: cached reuse must not open it.
                "source": self.base.join("absent-inputs").join(directory_key).to_string_lossy(),
                "sha256": sha256_file(&disk).unwrap(),
                "image_dir": self.config.emulator_image_dir,
                "image_identity": image_identity(&self.config).unwrap()
            })
        }

        fn write_record(&self, vm_dir: &Path, key: &str, record: &Value) {
            fs::write(vm_dir.join("boot").join(key).join("prepared.json"),
                serde_json::to_vec_pretty(record).unwrap()).unwrap();
        }

        fn cache(&self, vm_dir: &Path, root: &str, bytes: &[u8]) -> String {
            let key = root.replace('@', "-");
            let record = self.record(vm_dir, &key, root, bytes);
            let hash = record["sha256"].as_str().unwrap().to_string();
            self.write_record(vm_dir, &key, &record);
            hash
        }

        fn seed_versions(&self) -> (String, String) {
            (self.cache(&self.vm_dir, ROOT_A, b"prepared Magisk A fixture"),
             self.cache(&self.vm_dir, ROOT_B, b"prepared Magisk B fixture"))
        }
    }

    #[test]
    fn prepared_boot_switch_a_b_a_reuses_private_hashes_without_external_input() {
        let fixture = Fixture::new("switch");
        let (hash_a, hash_b) = fixture.seed_versions();
        assert_ne!(hash_a, hash_b);
        let initial = list_prepared_boots_in_dir(&fixture.config, &fixture.vm_dir).unwrap();
        assert_eq!(initial.iter().map(|entry| entry.root.as_str()).collect::<Vec<_>>(),
            vec![ROOT_A, ROOT_B]);
        let mut config = fixture.config.clone();
        for (root, hash) in [(ROOT_A, &hash_a), (ROOT_B, &hash_b), (ROOT_A, &hash_a)] {
            config.root_manager = root.into();
            assert!(config.prepared_root.is_empty());
            assert!(Path::new(&config.prepared_ramdisk).starts_with(&fixture.base));
            assert!(!Path::new(&config.prepared_ramdisk).exists());
            prepare_boot_in_dir(&config, &fixture.vm_dir).unwrap();
            assert_eq!(sha256_file(&fixture.vm_dir.join("boot/ramdisk.img")).unwrap(), *hash);
            let entries = list_prepared_boots_in_dir(&config, &fixture.vm_dir).unwrap();
            assert_eq!(entries.len(), 2);
            let selected = entries.iter().find(|entry| entry.root == root).unwrap();
            assert_eq!(&selected.sha256, hash);
            assert_eq!(sha256_file(Path::new(&selected.ramdisk)).unwrap(), *hash);
            assert!(!Path::new(&selected.source).exists());
            let other = entries.iter().find(|entry| entry.root != root).unwrap();
            assert_eq!(&other.sha256, if root == ROOT_A { &hash_b } else { &hash_a });
        }
    }

    #[test]
    fn prepared_boot_index_rejects_changed_image_fingerprints() {
        for changed in ["ramdisk.img", "build.prop"] {
            let fixture = Fixture::new("fingerprint");
            fixture.seed_versions();
            assert_eq!(list_prepared_boots_in_dir(&fixture.config, &fixture.vm_dir).unwrap().len(), 2);
            fs::write(fixture.image.join(changed), b"changed image fingerprint fixture").unwrap();
            assert!(list_prepared_boots_in_dir(&fixture.config, &fixture.vm_dir).unwrap().is_empty());
            assert!(prepare_boot_in_dir(&fixture.config, &fixture.vm_dir).is_err());
            assert!(!fixture.vm_dir.join("boot/ramdisk.img").exists());
        }
    }

    #[test]
    fn prepared_boot_index_rejects_tampered_ramdisk_and_private_fallback() {
        let fixture = Fixture::new("tampered");
        fixture.seed_versions();
        let disk = fixture.vm_dir.join("boot/magisk-28.0/ramdisk.img");
        fs::write(&disk, b"tampered private cache fixture").unwrap();
        let entries = list_prepared_boots_in_dir(&fixture.config, &fixture.vm_dir).unwrap();
        assert_eq!(entries.iter().map(|entry| entry.root.as_str()).collect::<Vec<_>>(), vec![ROOT_B]);
        let mut config = fixture.config.clone();
        config.prepared_root = ROOT_A.into();
        config.prepared_ramdisk = disk.to_string_lossy().into_owned();
        // Even matching prepared_root must not reclassify a bad private cache as external input.
        assert!(prepare_boot_in_dir(&config, &fixture.vm_dir).is_err());
        assert!(!fixture.vm_dir.join("boot/ramdisk.img").exists());
        assert_eq!(fs::read(disk).unwrap(), b"tampered private cache fixture");
    }

    #[test]
    fn prepared_boot_cache_isolated_between_vm_directories() {
        let fixture = Fixture::new("vm-isolation");
        let (hash_a, _) = fixture.seed_versions();
        let other_dir = fixture.base.join("vm-b");
        fs::create_dir(&other_dir).unwrap();
        let mut other = fixture.config.clone();
        other.id = "vm-b".into();
        assert!(list_prepared_boots_in_dir(&other, &other_dir).unwrap().is_empty());
        assert!(prepare_boot_in_dir(&other, &other_dir).is_err());
        assert!(!other_dir.join("boot").exists());
        let other_hash = fixture.cache(&other_dir, ROOT_A, b"different VM A fixture");
        assert_ne!(hash_a, other_hash);
        prepare_boot_in_dir(&other, &other_dir).unwrap();
        assert_eq!(sha256_file(&other_dir.join("boot/ramdisk.img")).unwrap(), other_hash);
        assert!(!fixture.vm_dir.join("boot/ramdisk.img").exists());
        prepare_boot_in_dir(&fixture.config, &fixture.vm_dir).unwrap();
        assert_eq!(sha256_file(&fixture.vm_dir.join("boot/ramdisk.img")).unwrap(), hash_a);
        assert_eq!(sha256_file(&other_dir.join("boot/ramdisk.img")).unwrap(), other_hash);
    }

    #[test]
    fn prepared_boot_index_filters_wrong_root_key_variant_and_corrupt_json() {
        let fixture = Fixture::new("invalid-index");
        let (hash_a, hash_b) = fixture.seed_versions();
        let cases = [
            ("magisk-30.0", json!("ksu@30.0"), json!("magisk-30.0")),
            ("magisk-31.0", json!("magisk@../31.0"), json!("magisk-31.0")),
            ("magisk-32.0", json!("magisk@33.0"), json!("magisk-32.0")),
            ("magisk-34.0", json!("magisk@34.0"), json!("wrong-key")),
            ("magisk-35.0", json!("magisk@35.0"), json!(35)),
            ("magisk-36.0", json!("magisk@36.0-debug"), json!("magisk-36.0")),
            ("magisk-37.0-debug", json!("magisk@37.0"), json!("magisk-37.0-debug")),
        ];
        for (key, root, recorded_key) in cases {
            let mut record = fixture.record(&fixture.vm_dir, key, ROOT_A, b"invalid record fixture");
            record["root"] = root;
            record["key"] = recorded_key;
            fixture.write_record(&fixture.vm_dir, key, &record);
        }
        let corrupt_key = "magisk-38.0";
        fixture.record(&fixture.vm_dir, corrupt_key, "magisk@38.0", b"corrupt JSON fixture");
        fs::write(fixture.vm_dir.join("boot").join(corrupt_key).join("prepared.json"), b"{broken-json").unwrap();
        let entries = list_prepared_boots_in_dir(&fixture.config, &fixture.vm_dir).unwrap();
        assert_eq!(entries.iter().map(|entry| (entry.root.as_str(), entry.sha256.as_str())).collect::<Vec<_>>(),
            vec![(ROOT_A, hash_a.as_str()), (ROOT_B, hash_b.as_str())]);
    }

    #[test]
    fn prepared_boot_rejects_unprepared_root_variant_without_reusing_base() {
        let fixture = Fixture::new("root-variant");
        fixture.seed_versions();
        let mut config = fixture.config.clone();
        config.root_manager = "magisk@28.0-debug".into();
        assert!(prepare_boot_in_dir(&config, &fixture.vm_dir).is_err());
        assert!(!fixture.vm_dir.join("boot/ramdisk.img").exists());
        assert!(!fixture.vm_dir.join("boot/magisk-28.0-debug").exists());
    }

    #[test]
    fn prepared_boot_legacy_record_without_key_upgrades_on_reuse() {
        let fixture = Fixture::new("legacy");
        let key = ROOT_A.replace('@', "-");
        let mut record = fixture.record(&fixture.vm_dir, &key, ROOT_A, b"legacy cached fixture");
        let expected_hash = record["sha256"].as_str().unwrap().to_string();
        record.as_object_mut().unwrap().remove("key");
        record.as_object_mut().unwrap().remove("image_identity");
        fixture.write_record(&fixture.vm_dir, &key, &record);
        assert_eq!(list_prepared_boots_in_dir(&fixture.config, &fixture.vm_dir).unwrap().len(), 1);
        prepare_boot_in_dir(&fixture.config, &fixture.vm_dir).unwrap();
        assert_eq!(sha256_file(&fixture.vm_dir.join("boot/ramdisk.img")).unwrap(), expected_hash);
        let upgraded: Value = serde_json::from_slice(&fs::read(
            fixture.vm_dir.join("boot").join(&key).join("prepared.json")).unwrap()).unwrap();
        assert_eq!(upgraded["key"], json!(key));
        assert_eq!(upgraded["image_identity"], image_identity(&fixture.config).unwrap());
        assert_eq!(upgraded["source"], record["source"]);
    }
}

pub async fn check_acceleration(c: &VmConfig) -> Result<()> {
    if c.sdk_path.trim().is_empty() || !emulator_exe(c).is_file() {
        return Err(Error::Other("请配置含 emulator/emulator.exe 的 Android SDK 目录".into()));
    }
    let mut command = Command::new(emulator_exe(c));
    command.arg("-accel-check").stdin(Stdio::null());
    #[cfg(windows)] { command.creation_flags(0x0800_0000); }
    command.kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(30), command.output()).await
        .map_err(|_| Error::Other("加速检测超时（30s），未改动驱动".into()))??;
    let text = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    if !output.status.success() { return Err(Error::Other(format!("加速检测失败（未修改驱动）: {text}"))); }
    let lower = text.to_lowercase();
    if c.accel == "aehd" && !(lower.contains("aehd") && lower.contains("usable") && !lower.contains("not usable")) {
        return Err(Error::Other(format!("选择了 AEHD，但检测结果未确认 AEHD: {text}")));
    }
    Ok(())
}

fn config_value<'a>(text: &'a str, wanted: &str) -> Option<&'a str> {
    // INI/properties permit whitespace around '='. Last assignment wins.
    text.lines().filter_map(|line| {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') { return None; }
        let (key, value) = line.split_once('=')?;
        (key.trim() == wanted).then_some(value.trim())
    }).last()
}

#[cfg(test)]
mod emulator_config_tests {
    use super::config_value;

    #[test]
    fn ini_values_accept_sdk_spacing_and_crlf() {
        assert_eq!(config_value("abi.type = x86_64\r\nhw.cpu.ncore = 4\r\n", "abi.type"), Some("x86_64"));
        assert_eq!(config_value("abi.type=x86_64\n", "abi.type"), Some("x86_64"));
        assert_eq!(config_value("ro.product.cpu.abi = x86_64\n", "ro.product.cpu.abi"), Some("x86_64"));
    }

    #[test]
    fn ini_values_do_not_trust_comments_or_shadowed_abis() {
        assert_eq!(config_value("# abi.type=x86_64\n;abi.type=x86_64\n", "abi.type"), None);
        assert_eq!(config_value("abi.type=x86_64\nabi.type=arm64-v8a\n", "abi.type"), Some("arm64-v8a"));
        assert_eq!(config_value("not.abi.type=x86_64\n", "abi.type"), None);
    }
}

fn data_partition_size(text: &str) -> Result<String> {
    let value = config_value(text, "disk.dataPartition.size").unwrap_or("6442450944");
    let (digits, multiplier) = match value.as_bytes().last().copied() {
        Some(b'M' | b'm') => (&value[..value.len() - 1], 1024_u64 * 1024),
        Some(b'G' | b'g') => (&value[..value.len() - 1], 1024_u64 * 1024 * 1024),
        _ => (value, 1),
    };
    let bytes = if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        digits.parse::<u64>().ok().and_then(|n| n.checked_mul(multiplier))
    } else { None };
    match bytes {
        Some(size) if (1024_u64 * 1024 * 1024..=1024_u64 * 1024 * 1024 * 1024).contains(&size) => Ok(size.to_string()),
        _ => Err(Error::Other("源AVD的disk.dataPartition.size须为1GiB至1TiB的字节数或M/G容量；未修改任何已有数据盘".into())),
    }
}

#[cfg(test)]
mod data_partition_tests {
    use super::data_partition_size;

    #[test]
    fn preserve_sdk_capacity_or_use_explicit_default() {
        assert_eq!(data_partition_size("").unwrap(), "6442450944");
        for value in ["6442450944", "6G", "6144M"] {
            assert_eq!(data_partition_size(&format!("disk.dataPartition.size = {value}\r\n")).unwrap(), "6442450944");
        }
    }

    #[test]
    fn reject_small_overflowing_or_injected_capacity() {
        for value in ["0", "1048576", "999999999999999999999G", "-6G", "6 G"] {
            assert!(data_partition_size(&format!("disk.dataPartition.size={value}")).is_err(), "{value}");
        }
        assert!(data_partition_size("disk.dataPartition.size=6G\ndisk.dataPartition.size=garbage").is_err());
    }

    // Disposable CI fixtures only; never resolve project_root(), c.dir(), or SDK tools.
    fn emulator_fixture(label: &str) -> (crate::types::VmConfig, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("roost-data-{label}-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&base).unwrap();
        let image = base.join("image");
        let source_home = base.join("source-avd");
        std::fs::create_dir(&image).unwrap();
        std::fs::create_dir_all(source_home.join("fixture.avd")).unwrap();
        for name in ["system.img", "ramdisk.img", "userdata.img", "encryptionkey.img"] {
            std::fs::write(image.join(name), format!("read-only fixture {name}")).unwrap();
        }
        std::fs::write(image.join("build.prop"), "ro.product.cpu.abi=x86_64\n").unwrap();
        std::fs::write(source_home.join("fixture.avd/config.ini"), "abi.type=x86_64\ndisk.dataPartition.size=6G\ndisk.dataPartition.path=must-not-inherit\ndisk.encryptionKeyPartition.path=must-not-inherit\n").unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "id":"data-fixture", "name":"isolated data fixture", "backend":"android-emulator",
            "root_manager":"none", "sdk_path":base.join("sdk").to_string_lossy(),
            "avd_home":source_home.to_string_lossy(), "avd_name":"fixture",
            "emulator_image_dir":image.to_string_lossy()
        })).unwrap();
        (config, base.join("private-vm"))
    }

    #[test]
    fn fresh_data_leaves_userdata_and_keys_absent_for_emulator_initialization() {
        let (config, private) = emulator_fixture("absent");
        let source = std::path::Path::new(&config.avd_home).join("fixture.avd/config.ini");
        let source_before = std::fs::read(&source).unwrap();
        super::prepare_emulator_data_in_dir(&config, &private).unwrap();
        assert!(!private.join("emulator/userdata-qemu.img").exists());
        assert!(!private.join("emulator/encryptionkey.img").exists());
        assert!(private.join("emulator/image-origin.json").is_file());
        let prepared = std::fs::read_to_string(private.join("avd/fixture.avd/config.ini")).unwrap();
        assert!(prepared.contains("disk.dataPartition.size=6442450944"));
        assert!(!prepared.contains("must-not-inherit"));
        assert_eq!(std::fs::read(source).unwrap(), source_before);
    }

    #[test]
    fn existing_uninitialized_seed_is_rejected_without_overwriting_data_or_key() {
        let (config, private) = emulator_fixture("seed");
        let data = private.join("emulator");
        std::fs::create_dir_all(&data).unwrap();
        let seed = std::fs::read(std::path::Path::new(&config.emulator_image_dir).join("userdata.img")).unwrap();
        std::fs::write(data.join("userdata-qemu.img"), &seed).unwrap();
        std::fs::write(data.join("encryptionkey.img"), b"existing private key").unwrap();
        let error = super::prepare_emulator_data_in_dir(&config, &private).unwrap_err().to_string();
        assert!(error.contains("userdata"));
        assert_eq!(std::fs::read(data.join("userdata-qemu.img")).unwrap(), seed);
        assert_eq!(std::fs::read(data.join("encryptionkey.img")).unwrap(), b"existing private key");
        assert!(!data.join("image-origin.json").exists());
    }

    #[test]
    fn initialized_same_length_data_and_key_are_not_replaced_on_repeat_preparation() {
        let (config, private) = emulator_fixture("preserve");
        let data = private.join("emulator");
        std::fs::create_dir_all(&data).unwrap();
        let seed_len = std::fs::metadata(std::path::Path::new(&config.emulator_image_dir).join("userdata.img")).unwrap().len() as usize;
        let initialized = vec![0x5a; seed_len];
        std::fs::write(data.join("userdata-qemu.img"), &initialized).unwrap();
        std::fs::write(data.join("encryptionkey.img"), b"keep private key").unwrap();
        for _ in 0..2 { super::prepare_emulator_data_in_dir(&config, &private).unwrap(); }
        assert_eq!(std::fs::read(data.join("userdata-qemu.img")).unwrap(), initialized);
        assert_eq!(std::fs::read(data.join("encryptionkey.img")).unwrap(), b"keep private key");
    }

    #[test]
    fn mismatched_image_identity_retains_origin_and_existing_disk() {
        let (config, private) = emulator_fixture("identity");
        let data = private.join("emulator");
        std::fs::create_dir_all(&data).unwrap();
        let origin = br#"{"image":"another image","build_prop_sha256":"another digest"}"#;
        std::fs::write(data.join("image-origin.json"), origin).unwrap();
        std::fs::write(data.join("userdata-qemu.img"), b"existing initialized data").unwrap();
        assert!(super::prepare_emulator_data_in_dir(&config, &private).is_err());
        assert_eq!(std::fs::read(data.join("image-origin.json")).unwrap(), origin);
        assert_eq!(std::fs::read(data.join("userdata-qemu.img")).unwrap(), b"existing initialized data");
    }
}

pub fn prepare_emulator_data(c: &VmConfig) -> Result<()> {
    prepare_emulator_data_in_dir(c, &c.dir())
}

/// Guest ABIs this backend can boot. arm64 runs under emulator TCG translation,
/// so it needs no host hypervisor and stays usable where AEHD is unstable.
const SUPPORTED_EMULATOR_ABIS: [&str; 2] = ["x86_64", "arm64-v8a"];

// Production uses c.dir(); tests provide isolated temp directories, never real VMs.
fn prepare_emulator_data_in_dir(c: &VmConfig, vm_dir: &Path) -> Result<()> {
    c.validate()?;
    validate_component(&c.avd_name)?;
    for value in [&c.sdk_path, &c.avd_home, &c.emulator_image_dir] {
        if !Path::new(value).is_absolute() { return Err(Error::Other("SDK/源AVD/镜像目录必须配置绝对路径".into())); }
    }
    let image = PathBuf::from(&c.emulator_image_dir);
    for name in ["system.img", "ramdisk.img", "userdata.img"] {
        if !image.join(name).is_file() { return Err(Error::PathNotFound(image.join(name))); }
    }
    // The image and the source AVD must agree on one supported ABI. arm64 guests
    // are allowed because the emulator runs them under software TCG translation.
    let prop = std::fs::read_to_string(image.join("build.prop")).unwrap_or_default();
    let image_abi = config_value(&prop, "ro.product.cpu.abi")
        .or_else(|| config_value(&prop, "ro.system.product.cpu.abi"))
        .ok_or_else(|| Error::Other("Emulator 镜像 build.prop 未声明 ro.product.cpu.abi".into()))?;
    if !SUPPORTED_EMULATOR_ABIS.contains(&image_abi) {
        return Err(Error::Other(format!("Emulator 镜像 ABI 暂不支持: {image_abi}")));
    }
    // Copy only the selected AVD metadata; all mutable paths stay under this VM.
    let source_avd = PathBuf::from(&c.avd_home).join(format!("{}.avd", c.avd_name)).join("config.ini");
    if !source_avd.is_file() { return Err(Error::PathNotFound(source_avd)); }
    let source_config = std::fs::read_to_string(&source_avd)?;
    let avd_abi = config_value(&source_config, "abi.type")
        .ok_or_else(|| Error::Other("源AVD config.ini未声明abi.type".into()))?;
    if avd_abi != image_abi {
        return Err(Error::Other(format!("源AVD abi.type={avd_abi} 与镜像 ABI={image_abi} 不一致")));
    }
    let avd_dir = vm_dir.join("avd").join(format!("{}.avd", c.avd_name));
    std::fs::create_dir_all(&avd_dir)?;
    let mut config = source_config.lines()
        .filter(|l| {
            let key = l.split('=').next().unwrap_or("").trim();
            !key.starts_with("disk.") && !key.starts_with("snapshot.") && !key.starts_with("sdcard.")
                && !key.starts_with("skin.") && !key.contains("path") && !key.starts_with("image.sysdir.")
                && key != "hw.sdCard" && key != "fastboot.forceChosenSnapshotBoot"
        })
        .collect::<Vec<_>>().join("\n");
    let capacity = data_partition_size(&source_config)?;
    config.push_str(&format!("\nimage.sysdir.1={}\ndisk.dataPartition.size={capacity}\nhw.sdCard=no\nfastboot.forceColdBoot=yes\n", image.to_string_lossy().replace('\\', "/")));
    std::fs::write(avd_dir.join("config.ini"), config)?;
    std::fs::write(vm_dir.join("avd").join(format!("{}.ini", c.avd_name)),
        format!("avd.ini.encoding=UTF-8\npath={}\n", avd_dir.to_string_lossy()))?;
    let data = vm_dir.join("emulator");
    std::fs::create_dir_all(&data)?;
    let identity_path = data.join("image-origin.json");
    let identity = serde_json::json!({ "image": image.canonicalize()?.to_string_lossy(), "build_prop_sha256": sha256_file(&image.join("build.prop"))? });
    if identity_path.is_file() {
        let existing: serde_json::Value = serde_json::from_slice(&std::fs::read(&identity_path)?)?;
        if existing != identity { return Err(Error::Other("该VM数据盘属于另一镜像；请新建VM以避免混用userdata/加密密钥".into())); }
    }
    let userdata = data.join("userdata-qemu.img");
    if userdata.is_file()
        && std::fs::metadata(&userdata)?.len() == std::fs::metadata(image.join("userdata.img"))?.len()
        && sha256_file(&userdata)? == sha256_file(&image.join("userdata.img"))? {
        return Err(Error::Other("该VM数据盘仍是未经Emulator初始化的userdata种子，存在/data挂载失败；文件已保留，请新建VM。应用不会自动删除或覆盖既有数据盘".into()));
    }
    // An absent -data destination is intentional: Emulator initializes/formats it
    // from the system image and owns the matching encryption metadata. A direct
    // copy of the 1 MiB seed falsely looks initialized and fails to mount /data.
    std::fs::write(identity_path, serde_json::to_vec_pretty(&identity)?)?;
    Ok(())
}

pub fn emulator_command(c: &VmConfig, headless: bool) -> Result<Vec<String>> {
    c.validate()?;
    validate_component(&c.avd_name)?;
    if c.sdk_path.is_empty() || c.emulator_image_dir.is_empty() { return Err(Error::Other("SDK/系统镜像目录尚未配置".into())); }
    let data = c.dir().join("emulator");
    let image = PathBuf::from(&c.emulator_image_dir);
    let kernel = ["kernel-ranchu", "kernel-ranchu-64", "kernel"].iter().map(|v| image.join(v))
        .find(|p| p.is_file()).ok_or_else(|| Error::Other("镜像目录缺少 Emulator kernel".into()))?;
    let mut args = vec![emulator_exe(c).to_string_lossy().into_owned(), "-avd".into(), c.avd_name.clone(),
        "-sysdir".into(), image.to_string_lossy().into_owned(),
        "-datadir".into(), data.to_string_lossy().into_owned(),
        "-data".into(), data.join("userdata-qemu.img").to_string_lossy().into_owned(),
        "-cache".into(), data.join("cache.img").to_string_lossy().into_owned(),
        "-kernel".into(), kernel.to_string_lossy().into_owned(),
        "-system".into(), image.join("system.img").to_string_lossy().into_owned(),
        "-ramdisk".into(), c.dir().join("boot/ramdisk.img").to_string_lossy().into_owned(),
        "-port".into(), c.console_port.to_string(), "-accel".into(), emulator_accel(&c.accel)?.into(),
        "-memory".into(), c.memory_mb.to_string(), "-cores".into(), c.cores.to_string(),
        "-gpu".into(), "swiftshader_indirect".into(), "-no-snapshot".into()];
    if image.join("vendor.img").is_file() { args.extend(["-vendor".into(), image.join("vendor.img").to_string_lossy().into_owned()]); }
    if headless { args.extend(["-no-window".into(), "-no-audio".into()]); }
    Ok(args)
}

/// Emulator accepts only on/off. "off" is the explicit opt-out that keeps a host
/// hypervisor driver (for example AEHD) out of the boot path entirely.
fn emulator_accel(value: &str) -> Result<&'static str> {
    match value {
        "on" | "auto" | "kvm" | "hax" | "hvf" => Ok("on"),
        "off" | "tcg" => Ok("off"),
        other => Err(Error::Other(format!("Emulator 加速仅支持 on/auto/kvm/hax/hvf 或 off/tcg: {other}"))),
    }
}

#[cfg(test)]
mod emulator_accel_tests {
    use super::emulator_accel;

    // Only explicit software selections may drop hardware acceleration.
    #[test]
    fn acceleration_requires_an_explicit_selection() {
        assert_eq!(emulator_accel("on").unwrap(), "on");
        assert_eq!(emulator_accel("auto").unwrap(), "on");
        assert_eq!(emulator_accel("kvm").unwrap(), "on");
        assert_eq!(emulator_accel("hvf").unwrap(), "on");
        assert_eq!(emulator_accel("off").unwrap(), "off");
        assert_eq!(emulator_accel("tcg").unwrap(), "off");
        for rejected in ["", "aehd", "ON", "on,thread=multi", " off", "0"] {
            assert!(emulator_accel(rejected).is_err(), "unexpected acceptance: {rejected:?}");
        }
    }
}

fn require_ok(result: CmdResult) -> Result<CmdResult> {
    if result.ok { Ok(result) } else { Err(Error::Command(result.output())) }
}

#[derive(Debug, Serialize)]
pub struct ModuleInstallResult {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
    pub needs_reboot: bool,
    pub module_id: String,
}

const DISABLE_LSPOSED_SCRIPT: &str = include_str!("disable-lsposed.sh");

fn disable_staged(result: &CmdResult) -> bool {
    result.ok && result.code == 0 && result.stdout.trim() == "ROOST_LSPOSED_DISABLE_STAGED"
}

fn disabled_module_result(result: CmdResult) -> Result<ModuleInstallResult> {
    if !disable_staged(&result) {
        return Err(Error::Other(format!(
            "LSPosed禁用提交未核实；部分目录可能已设置标记，请重新核验。{}", result.output()
        )));
    }
    Ok(ModuleInstallResult { ok: true, stdout: result.stdout, stderr: result.stderr,
        code: result.code, needs_reboot: true, module_id: "lsposed-disabled".into() })
}

/// Caller holds the owned VM operation lease and explicit Shell authorization.
/// Staging all markers is not proof that the current framework has unloaded.
pub async fn disable_lsposed(adb: &Adb) -> Result<ModuleInstallResult> {
    disabled_module_result(adb.su_shell(DISABLE_LSPOSED_SCRIPT).await?)
}

#[cfg(test)]
mod disable_staging_tests {
    use super::*;

    fn observation(ok: bool, code: i32, stdout: &str) -> CmdResult {
        CmdResult { ok, code, stdout: stdout.into(), stderr: String::new() }
    }

    #[test]
    fn disable_staging_requires_exit_zero_and_one_exact_completion_marker() {
        assert!(disable_staged(&observation(true, 0, "ROOST_LSPOSED_DISABLE_STAGED\n")));
        for (ok, code, output) in [
            (false, 0, "ROOST_LSPOSED_DISABLE_STAGED"),
            (true, 2, "ROOST_LSPOSED_DISABLE_STAGED"), (true, 0, ""),
            (true, 0, "ROOST_LSPOSED_DISABLE_STAGED\nROOST_LSPOSED_DISABLE_STAGED"),
            (true, 0, "prefix ROOST_LSPOSED_DISABLE_STAGED"),
            (true, 0, "ROOST_LSPOSED_DISABLE_STAGED\nError: failed"),
        ] { assert!(!disable_staged(&observation(ok, code, output))); }
    }

    #[test]
    fn staging_result_never_claims_live_unload_or_ignores_partial_failure() {
        let result = disabled_module_result(observation(true, 0, "ROOST_LSPOSED_DISABLE_STAGED\n")).unwrap();
        assert!(result.ok && result.needs_reboot);
        assert_eq!(result.module_id, "lsposed-disabled");
        assert!(disabled_module_result(observation(false, 2, "")).is_err());
        assert!(disabled_module_result(observation(true, 0, "")).is_err());
    }
}

#[derive(Debug)]
struct ModuleMetadata {
    id: String,
    version: Option<String>,
    version_code: Option<String>,
}

fn unique_module_property<'a>(prop: &'a str, key: &str) -> Result<Option<&'a str>> {
    let mut values = prop.lines().filter_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name.trim() == key).then_some((name, value))
    });
    let entry = values.next();
    if values.next().is_some() {
        return Err(Error::Other(format!("module.prop 重复字段: {key}")));
    }
    match entry {
        Some((name, value)) if name == key => Ok(Some(value)),
        Some(_) => Err(Error::Other(format!("module.prop 字段格式含糊: {key}"))),
        None => Ok(None),
    }
}

fn parse_module_metadata(prop: &str) -> Result<ModuleMetadata> {
    if prop.len() > 65536 || prop.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
        return Err(Error::Other("module.prop 过大或含无效控制字符".into()));
    }
    let id = unique_module_property(prop, "id")?.unwrap_or("");
    validate_component(id)?;
    if !id.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
        return Err(Error::Other("Magisk module id须以字母开头".into()));
    }
    let version = unique_module_property(prop, "version")?.map(str::to_string);
    let version_code = unique_module_property(prop, "versionCode")?;
    if version_code.map(|v| v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit())).unwrap_or(false) {
        return Err(Error::Other("module.prop versionCode 须为十进制整数".into()));
    }
    Ok(ModuleMetadata { id: id.into(), version, version_code: version_code.map(str::to_string) })
}

// zip 2.x indexes names in a map. Count bounded raw central entries first so
// repeated module.prop entries cannot disappear behind that name lookup.
fn require_unique_module_entry<R: std::io::Read + std::io::Seek>(reader: &mut R, start: u64, indexed_count: usize) -> Result<()> {
    use std::io::SeekFrom;
    reader.seek(SeekFrom::Start(start))?;
    let mut entries = 0usize;
    let mut properties = 0usize;
    let mut directory_bytes = 0usize;
    loop {
        let mut header = [0u8; 46];
        reader.read_exact(&mut header[..4])?;
        if &header[..4] == b"PK\x05\x06" || &header[..4] == b"PK\x06\x06" { break; }
        if &header[..4] != b"PK\x01\x02" { return Err(Error::Other("模块ZIP中央目录结构不受支持".into())); }
        reader.read_exact(&mut header[4..])?;
        let name_len = u16::from_le_bytes([header[28], header[29]]) as usize;
        let extra_len = u16::from_le_bytes([header[30], header[31]]) as usize;
        let comment_len = u16::from_le_bytes([header[32], header[33]]) as usize;
        entries += 1;
        directory_bytes += 46 + name_len + extra_len + comment_len;
        if entries > 4096 || directory_bytes > 16 * 1024 * 1024 {
            return Err(Error::Other("模块ZIP中央目录超过校验预算".into()));
        }
        let mut name = vec![0u8; name_len];
        reader.read_exact(&mut name)?;
        if name == b"module.prop" { properties += 1; }
        let mut extra = vec![0u8; extra_len];
        reader.read_exact(&mut extra)?;
        let mut cursor = 0usize;
        while cursor < extra.len() {
            if extra.len() - cursor < 4 { return Err(Error::Other("模块ZIP扩展字段截断".into())); }
            let tag = u16::from_le_bytes([extra[cursor], extra[cursor + 1]]);
            let size = u16::from_le_bytes([extra[cursor + 2], extra[cursor + 3]]) as usize;
            cursor += 4;
            if size > extra.len() - cursor { return Err(Error::Other("模块ZIP扩展字段长度无效".into())); }
            if tag == 0x7075 { return Err(Error::Other("模块ZIP含Unicode路径别名，元数据身份含糊".into())); }
            cursor += size;
        }
        reader.seek(SeekFrom::Current(comment_len as i64))?;
    }
    if properties != 1 || entries != indexed_count {
        return Err(Error::Other("模块ZIP条目重复或根目录module.prop不唯一".into()));
    }
    Ok(())
}

fn read_module_metadata<R: std::io::Read + std::io::Seek>(mut reader: R) -> Result<ModuleMetadata> {
    use std::io::Read;
    let archive = zip::ZipArchive::new(&mut reader).map_err(|e| Error::Other(format!("模块 ZIP 无效: {e}")))?;
    let start = archive.central_directory_start();
    let indexed_count = archive.len();
    drop(archive);
    require_unique_module_entry(&mut reader, start, indexed_count)?;
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| Error::Other(format!("模块 ZIP 无效: {e}")))?;
    let member = archive.by_name("module.prop").map_err(|e| Error::Other(format!("module.prop 读取失败: {e}")))?;
    let kind = member.unix_mode().map(|mode| mode & 0o170000).unwrap_or(0);
    if !member.is_file() || (kind != 0 && kind != 0o100000) || member.size() > 65536 {
        return Err(Error::Other("module.prop 须为不大于64KiB的普通文件".into()));
    }
    let mut prop = String::new();
    member.take(65537).read_to_string(&mut prop)?;
    parse_module_metadata(&prop)
}

fn normalized_module_version<'a>(actual: &'a str, code: Option<&str>) -> Option<&'a str> {
    if code.map(|v| v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit())).unwrap_or(false) { return None; }
    let actual = actual.strip_prefix('v').unwrap_or(actual);
    if let Some((base, suffix)) = actual.split_once(" (") {
        let build = suffix.strip_suffix(')')?;
        if base.is_empty() || base.bytes().any(|b| b.is_ascii_whitespace())
            || build.is_empty() || !build.bytes().all(|b| b.is_ascii_digit()) || code != Some(build) { return None; }
        Some(base)
    } else {
        if actual.is_empty() || actual.bytes().any(|b| b.is_ascii_whitespace()) { return None; }
        Some(actual)
    }
}

fn require_selected_lsposed(metadata: &ModuleMetadata, selection: &str) -> Result<()> {
    let (id, version) = match selection {
        "1.9.2-zygisk" => ("zygisk_lsposed", "1.9.2"),
        "1.9.2-riru" => ("riru_lsposed", "1.9.2"),
        _ => return Err(Error::Other("自动应用基线仅支持公开 LSPosed 1.9.2".into())),
    };
    let actual = metadata.version.as_deref().and_then(|value| normalized_module_version(value, metadata.version_code.as_deref()));
    if metadata.id != id || actual != Some(version) {
        return Err(Error::Other(format!("LSPosed缓存包与所选 {selection} 不匹配；尚未上传或安装")));
    }
    Ok(())
}

#[cfg(test)]
mod module_package_tests {
    use super::*;
    use std::io::Cursor;

    const RELEASE: &str = "id=zygisk_lsposed\nname=Zygisk - LSPosed\nversion=v1.9.2 (7024)\nversionCode=7024\n";

    #[test]
    fn selected_release_requires_matching_id_and_exact_version() {
        let metadata = parse_module_metadata(RELEASE).unwrap();
        assert!(require_selected_lsposed(&metadata, "1.9.2-zygisk").is_ok());
        for selection in ["1.9.2-riru", "1.9.20-zygisk", "none", ""] {
            assert!(require_selected_lsposed(&metadata, selection).is_err());
        }
        for prop in [RELEASE.replace("zygisk_lsposed", "other_module"),
            RELEASE.replace("1.9.2", "1.9.20"), RELEASE.replace("version=v1.9.2 (7024)\n", ""),
            RELEASE.replace("versionCode=7024", "versionCode=7025")] {
            let metadata = parse_module_metadata(&prop).unwrap();
            assert!(require_selected_lsposed(&metadata, "1.9.2-zygisk").is_err());
        }
        let riru = parse_module_metadata(&RELEASE.replace("zygisk_lsposed", "riru_lsposed")).unwrap();
        assert!(require_selected_lsposed(&riru, "1.9.2-riru").is_ok());
    }

    #[test]
    fn duplicate_and_invalid_metadata_is_not_silently_first_wins() {
        for extra in ["id=zygisk_lsposed\n", " id =riru_lsposed\n", "version=v1.9.2\n",
            "versionCode=7024\n", "versionCode=7025\n"] {
            assert!(parse_module_metadata(&format!("{RELEASE}{extra}")).is_err());
        }
        for prop in ["id=\n", "id=../outside\n", "id=123bad\n", "id=good\0name\n", "id=good\nversionCode=x\n",
            " id=good\n", "id =good\n", "id=good\n version=1.9.2\n", "id=good\nversionCode =7024\n"] {
            assert!(parse_module_metadata(prop).is_err());
        }
        assert!(parse_module_metadata(&"x".repeat(65537)).is_err());
        // Generic module installs do not require LSPosed-specific fields.
        assert_eq!(parse_module_metadata("id=custom_module\n").unwrap().id, "custom_module");
    }

    #[test]
    fn version_build_suffix_is_shared_with_runtime_observation() {
        assert_eq!(normalized_module_version("v1.9.2 (7024)", Some("7024")), Some("1.9.2"));
        assert_eq!(normalized_module_version("1.9.2", None), Some("1.9.2"));
        for (value, code) in [("1.9.2 (7024)", None), ("1.9.2 (7024)", Some("07024")),
            ("1.9.2  (7024)", Some("7024")), ("1.9.2 (7024) extra", Some("7024")),
            ("1.9.2\t", None), ("", None)] {
            assert_eq!(normalized_module_version(value, code), None);
        }
    }

    #[test]
    fn zip_reader_checks_actual_member_content_and_metadata_limit() {
        use std::io::Write;
        for (name, content, expected_ok) in [
            ("module.prop", RELEASE.as_bytes().to_vec(), true),
            ("nested/module.prop", RELEASE.as_bytes().to_vec(), false),
            ("module.prop", b"id=wrong\nid=zygisk_lsposed\n".to_vec(), false),
            ("module.prop", vec![b'x'; 65537], false),
            ("module.prop", vec![0xff], false),
        ] {
            let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
            writer.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(&content).unwrap();
            let data = writer.finish().unwrap().into_inner();
            let result = read_module_metadata(Cursor::new(data));
            assert_eq!(result.is_ok(), expected_ok, "{name}");
            if expected_ok { assert!(require_selected_lsposed(&result.unwrap(), "1.9.2-zygisk").is_ok()); }
        }
    }

    #[test]
    fn uploaded_digest_requires_success_and_exact_generated_path() {
        let digest = "a".repeat(64);
        let remote = "/data/local/tmp/roost-fixture-1.zip";
        let good = format!("{digest}  {remote}\n");
        let observation = |ok, code, stdout: String| CmdResult { ok, code, stdout, stderr: String::new() };
        assert!(uploaded_module_matches(&observation(true, 0, good.clone()), &digest, remote));
        for result in [observation(false, 0, good.clone()), observation(true, 1, good.clone()),
            observation(true, 0, format!("{good}extra\n")), observation(true, 0, format!("{good}{good}")),
            observation(true, 0, format!("{digest}  /data/local/tmp/other.zip\n")),
            observation(true, 0, format!("{}  {remote}\n", "b".repeat(64)))] {
            assert!(!uploaded_module_matches(&result, &digest, remote));
        }
    }

    #[test]
    fn archive_hash_rewinds_the_same_reader() {
        let mut reader = Cursor::new(b"abc".to_vec());
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(module_archive_digest(&mut reader).unwrap(), expected);
        assert_eq!(reader.position(), 0);
        assert_eq!(module_archive_digest(&mut reader).unwrap(), expected);
    }

    fn central_entries(names: &[&[u8]]) -> Vec<u8> {
        let mut data = Vec::new();
        for name in names {
            let mut header = [0u8; 46];
            header[..4].copy_from_slice(b"PK\x01\x02");
            header[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
            data.extend_from_slice(&header);
            data.extend_from_slice(name);
        }
        data.extend_from_slice(b"PK\x05\x06");
        data
    }

    #[test]
    fn raw_directory_count_rejects_duplicates_hidden_by_zip_name_map() {
        let valid = central_entries(&[b"module.prop", b"customize.sh"]);
        assert!(require_unique_module_entry(&mut Cursor::new(valid), 0, 2).is_ok());
        let duplicate = central_entries(&[b"module.prop", b"module.prop"]);
        assert!(require_unique_module_entry(&mut Cursor::new(duplicate), 0, 1).is_err());
        let other_duplicate = central_entries(&[b"module.prop", b"customize.sh", b"customize.sh"]);
        assert!(require_unique_module_entry(&mut Cursor::new(other_duplicate), 0, 2).is_err());
        for names in [vec![b"nested/module.prop".as_slice()], vec![b"MODULE.PROP".as_slice()], vec![]] {
            let count = names.len();
            assert!(require_unique_module_entry(&mut Cursor::new(central_entries(&names)), 0, count).is_err());
        }
    }

    #[test]
    fn raw_directory_rejects_unicode_path_aliases_and_malformed_extra_fields() {
        for extra in [vec![0x75, 0x70, 0, 0], vec![1, 0, 8, 0, 1], vec![1, 0, 0]] {
            let mut data = central_entries(&[b"module.prop"]);
            data[30..32].copy_from_slice(&(extra.len() as u16).to_le_bytes());
            data.splice(46 + b"module.prop".len()..46 + b"module.prop".len(), extra);
            assert!(require_unique_module_entry(&mut Cursor::new(data), 0, 1).is_err());
        }
        let mut data = central_entries(&[b"module.prop"]);
        data[30..32].copy_from_slice(&4u16.to_le_bytes());
        data.splice(46 + b"module.prop".len()..46 + b"module.prop".len(), [0x55, 0x54, 0, 0]);
        assert!(require_unique_module_entry(&mut Cursor::new(data), 0, 1).is_ok());
    }

    #[test]
    fn raw_directory_rejects_truncation_bad_signature_and_entry_budget() {
        for data in [vec![], b"BAD!".to_vec(), b"PK\x01\x02".to_vec()] {
            assert!(require_unique_module_entry(&mut Cursor::new(data), 0, 1).is_err());
        }
        let names = vec![b"module.prop".as_slice(); 4097];
        assert!(require_unique_module_entry(&mut Cursor::new(central_entries(&names)), 0, names.len()).is_err());
    }
}

pub async fn install_zip(adb: &Adb, c: &VmConfig, path: &str) -> Result<ModuleInstallResult> {
    install_zip_checked(adb, c, path, None).await
}

/// Validate the actual archive metadata before any device request. Cache filenames
/// and directory names never establish the package's identity or version.
pub async fn install_lsposed_zip(adb: &Adb, c: &VmConfig, path: &str, selection: &str) -> Result<ModuleInstallResult> {
    install_zip_checked(adb, c, path, Some(selection)).await
}

fn module_archive_digest<R: std::io::Read + std::io::Seek>(reader: &mut R) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::SeekFrom;
    reader.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
    }
    reader.seek(SeekFrom::Start(0))?;
    Ok(format!("{:x}", hash.finalize()))
}

fn uploaded_module_matches(result: &CmdResult, digest: &str, remote: &str) -> bool {
    if !result.ok || result.code != 0 || digest.len() != 64
        || !digest.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) { return false; }
    result.stdout.trim_end_matches(['\r', '\n']) == format!("{digest}  {remote}")
}

async fn install_zip_checked(adb: &Adb, c: &VmConfig, path: &str, selection: Option<&str>) -> Result<ModuleInstallResult> {
    let local = Path::new(path);
    if local.extension().and_then(|s| s.to_str()).map(|s| !s.eq_ignore_ascii_case("zip")).unwrap_or(true) {
        return Err(Error::Other("Magisk 安装器仅接收 .zip 模块包".into()));
    }
    let mut file = std::fs::File::open(local)?;
    if !file.metadata()?.is_file() { return Err(Error::Other("模块ZIP须为普通文件".into())); }
    let digest = module_archive_digest(&mut file)?;
    let metadata = read_module_metadata(&mut file)?;
    if let Some(selection) = selection { require_selected_lsposed(&metadata, selection)?; }
    if module_archive_digest(&mut file)? != digest {
        return Err(Error::Other("模块ZIP在校验期间发生变化；尚未上传或安装".into()));
    }
    let module_id = metadata.id;
    drop(file);
    if !adb.is_booted().await { return Err(Error::Other("VM 尚未完成启动".into())); }
    if !adb.check_root().await { return Err(Error::Other("VM 尚未验证 root；请先启动准备好的 Magisk ramdisk".into())); }
    require_ok(adb.su_shell(&format!("{} -v", c.magisk_path)).await?)?;
    let environment = adb.su_shell("test -x /data/adb/magisk/busybox && test -x /data/adb/magisk/magiskboot && test -f /data/adb/magisk/util_functions.sh").await?;
    if !environment.ok { return Err(Error::Other("该VM缺少Magisk运行资源；请先点击初始化Magisk运行环境（需已缓存对应完整APK）".into())); }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let remote = format!("/data/local/tmp/roost-{}-{stamp}.zip", c.id);
    let result: Result<CmdResult> = async {
        require_ok(adb.push(path, &remote).await?)?;
        let uploaded = adb.shell(&format!("sha256sum {remote}")).await?;
        if !uploaded_module_matches(&uploaded, &digest, &remote) {
            return Err(Error::Other("上传模块SHA256未核实；未运行模块安装器".into()));
        }
        adb.su_shell(&format!("{} --install-module {remote}", c.magisk_path)).await
    }.await;
    // Cleanup is limited to this operation's generated file on this exact serial.
    let _ = adb.shell(&format!("rm -f {remote}")).await;
    let result = require_ok(result?)?;
    // Magisk merges modules_update into modules on the next boot, and on this guest that
    // merge leaves most payload files as 0 bytes (observed: installed files were correct,
    // then module.prop/scripts/apks became empty and zygisk dlopen failed on a 0-byte
    // library). Publish the staged tree ourselves and verify every file before the reboot.
    let staged = format!("/data/adb/modules_update/{module_id}");
    let staged_probe = adb.su_shell(&format!("test -d {staged}")).await?;
    if staged_probe.ok {
        let published = adb.su_shell(&format!(
            "set -e; target=/data/adb/modules/{module_id}; rm -rf \"$target\"; mkdir -p \"$target\"; cp -a {staged}/. \"$target\"/; find \"$target\" -type f -size 0 | grep -q . && exit 3; rm -f \"$target/update\"; rm -rf {staged}; test -s \"$target/module.prop\""))
            .await?;
        if !published.ok {
            return Err(Error::Other(format!("模块文件搬运后校验失败（存在0字节文件）；未标记待重启。{}", published.output())));
        }
    }
    let state = adb.su_shell(&format!("test -s /data/adb/modules/{module_id}/module.prop")).await?;
    if !state.ok { return Err(Error::Other(format!("安装器退出成功但未发现可用模块 {module_id}；{}", result.output()))); }
    // Magisk on this guest wipes the module directory contents during boot, and it also
    // deleted a backup directory whose name matched the module id. Store the payload as a
    // single archive under a name that cannot collide with any module id, then restore it
    // from an early boot hook that lives outside the module tree.
    let backup = format!("/data/adb/roost-mb-{}.tar", &digest[..16]);
    let restore = format!("/data/adb/post-fs-data.d/roost-rb-{}.sh", &digest[..16]);
    // The restore script is staged as a file: a multi-line heredoc inside su -c
    // broke shell quoting on this guest ("no closing quote"). Push, then place it.
    let local_stage = std::env::temp_dir().join(format!("roost-rb-{}.sh", &digest[..16]));
    let script = format!("#!/system/bin/sh\nset -e\nsrc={backup}\ntarget=/data/adb/modules/{module_id}\ntmp=/data/adb/roost-rt-{0}\n[ -s \"$src\" ] || exit 0\nrm -rf \"$tmp\"\nmkdir -p \"$tmp\"\ntar -xf \"$src\" -C \"$tmp\"\n[ -s \"$tmp/module.prop\" ] || exit 0\nrm -rf \"$target\"\nmkdir -p \"$target\"\ncp -a \"$tmp\"/. \"$target\"/\nrm -rf \"$tmp\"\n", &digest[..16]);
    std::fs::write(&local_stage, script)?;
    let remote_stage = format!("/data/local/tmp/roost-rb-{}.sh", &digest[..16]);
    let backup_result: Result<CmdResult> = async {
        require_ok(adb.push(&local_stage.to_string_lossy(), &remote_stage).await?)?;
        require_ok(adb.su_shell(&format!("( cd /data/adb/modules/{module_id} && tar -cf {backup} . )")).await?)?;
        require_ok(adb.su_shell("mkdir -p /data/adb/post-fs-data.d").await?)?;
        require_ok(adb.su_shell(&format!("cp {remote_stage} {restore} && chmod 755 {restore} && chmod 755 /data/adb/post-fs-data.d")).await?)?;
        require_ok(adb.su_shell(&format!("test -s {backup} && test -s {restore}")).await?)
    }.await;
    let _ = std::fs::remove_file(&local_stage);
    let _ = adb.shell(&format!("rm -f {remote_stage}")).await;
    let backup_result = backup_result?;
    if !backup_result.ok {
        return Err(Error::Other(format!("模块启动恢复副本创建失败；未标记待重启。{}", backup_result.output())));
    }
    Ok(ModuleInstallResult { ok: true, stdout: result.stdout, stderr: result.stderr,
        code: result.code, needs_reboot: true, module_id })
}

#[derive(Debug, Serialize)]
pub struct VerifyVmResult {
    pub root_verified: bool,
    pub lsp_verified: bool,
    pub lsp_installed: bool,
    pub lsp_state: String,
    pub pending_restart: bool,
    pub message: String,
    pub boot_id: String,
}

fn process_probe(name: &str) -> String {
    // Only fixed internal process names are accepted; never interpolate user input.
    debug_assert!(name == "magiskd" || name == "lspd");
    format!(r#"command -v pidof >/dev/null 2>&1 || exit 127; pids=$(pidof {name}); code=$?; if [ "$code" -eq 0 ] && [ -n "$pids" ]; then printf 'ROOST_PROCESS_PRESENT\n'; elif [ "$code" -eq 1 ]; then printf 'ROOST_PROCESS_ABSENT\n'; else exit 2; fi"#)
}

fn observed_process(result: &CmdResult) -> Option<bool> {
    if !result.ok || result.code != 0 { return None; }
    match result.stdout.trim() {
        "ROOST_PROCESS_PRESENT" => Some(true),
        "ROOST_PROCESS_ABSENT" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod process_observation_tests {
    use super::{observed_process, CmdResult};

    fn observation(ok: bool, stdout: &str, stderr: &str) -> CmdResult {
        CmdResult { ok, stdout: stdout.into(), stderr: stderr.into(), code: if ok { 0 } else { 1 } }
    }

    // Only successful, unambiguous guest markers establish presence or absence.
    #[test]
    fn successful_exact_process_markers_are_required() {
        assert_eq!(observed_process(&observation(true, "ROOST_PROCESS_PRESENT\n", "")), Some(true));
        assert_eq!(observed_process(&observation(true, " \nROOST_PROCESS_ABSENT\n ", "")), Some(false));
        for stdout in ["", "123", "unknown", "ROOST_PROCESS_ABSENT\nROOST_PROCESS_PRESENT", "prefix ROOST_PROCESS_ABSENT"] {
            assert_eq!(observed_process(&observation(true, stdout, "")), None, "ambiguous output: {stdout:?}");
        }
    }

    #[test]
    fn failed_command_is_never_proof_of_process_absence() {
        for stdout in ["", "ROOST_PROCESS_ABSENT", "ROOST_PROCESS_PRESENT"] {
            assert_eq!(observed_process(&observation(false, stdout, "error: device offline")), None);
        }
        assert_eq!(observed_process(&observation(false, "", "pidof: not found")), None);
    }
}

/// Fixed, privileged directory observations. A missing base is known only when its
/// readable/searchable parent was listed successfully. Any failure emits no marker.
fn modules_probe(staged: bool) -> String {
    let base = if staged { "/data/adb/modules_update" } else { "/data/adb/modules" };
    format!(r#"base={base}; [ -r /data/adb ] && [ -x /data/adb ] && ls -A /data/adb >/dev/null || exit 2; [ ! -L "$base" ] || exit 2; if [ ! -e "$base" ]; then printf 'ROOST_MODULES_DISABLED\n'; exit 0; fi; [ -d "$base" ] && [ -r "$base" ] && [ -x "$base" ] && ls -A "$base" >/dev/null || exit 2; enabled=0; for name in zygisk_lsposed riru_lsposed; do d="$base/$name"; [ ! -L "$d" ] || exit 2; if [ -e "$d" ]; then [ -d "$d" ] && [ -r "$d" ] && [ -x "$d" ] && ls -A "$d" >/dev/null || exit 2; if [ ! -e "$d/disable" ] && [ ! -L "$d/disable" ] && [ ! -e "$d/remove" ] && [ ! -L "$d/remove" ]; then enabled=1; fi; fi; done; if [ "$enabled" -eq 1 ]; then printf 'ROOST_MODULES_ENABLED\n'; else printf 'ROOST_MODULES_DISABLED\n'; fi"#)
}

fn observed_modules(result: &CmdResult) -> Option<bool> {
    if !result.ok || result.code != 0 { return None; }
    match result.stdout.trim() {
        "ROOST_MODULES_ENABLED" => Some(true),
        "ROOST_MODULES_DISABLED" => Some(false),
        _ => None,
    }
}

fn selected_module_probe(module_id: &str) -> String {
    debug_assert!(matches!(module_id, "zygisk_lsposed" | "riru_lsposed"));
    format!(r#"base=/data/adb/modules; [ -r /data/adb ] && [ -x /data/adb ] && ls -A /data/adb >/dev/null || exit 2; [ ! -L "$base" ] || exit 2; if [ ! -e "$base" ]; then printf 'ROOST_SELECTED_ABSENT\n'; exit 0; fi; [ -d "$base" ] && [ -r "$base" ] && [ -x "$base" ] && ls -A "$base" >/dev/null || exit 2; d="$base/{module_id}"; [ ! -L "$d" ] || exit 2; if [ ! -e "$d" ]; then printf 'ROOST_SELECTED_ABSENT\n'; exit 0; fi; [ -d "$d" ] && [ -r "$d" ] && [ -x "$d" ] && ls -A "$d" >/dev/null || exit 2; if [ -e "$d/disable" ] || [ -L "$d/disable" ] || [ -e "$d/remove" ] || [ -L "$d/remove" ]; then printf 'ROOST_SELECTED_ABSENT\n'; exit 0; fi; [ -f "$d/module.prop" ] && [ -r "$d/module.prop" ] || exit 2; printf 'ROOST_SELECTED_BEGIN\n'; cat "$d/module.prop" || exit 2; printf '\nROOST_SELECTED_END\n'"#)
}

/// None means unreadable/malformed; false means an observed absent/other selection.
fn observed_selected_module(result: &CmdResult, module_id: &str, version: &str) -> Option<bool> {
    if !result.ok || result.code != 0 { return None; }
    let output = result.stdout.trim();
    if output == "ROOST_SELECTED_ABSENT" { return Some(false); }
    let body = output.strip_prefix("ROOST_SELECTED_BEGIN\n")?.strip_suffix("\nROOST_SELECTED_END")?;
    if body.lines().any(|line| line.starts_with("ROOST_SELECTED_")) { return None; }
    let metadata = parse_module_metadata(body).ok()?;
    let actual = metadata.version.as_deref()?;
    let base = normalized_module_version(actual, metadata.version_code.as_deref())?;
    let expected = version.strip_prefix('v').unwrap_or(version);
    Some(metadata.id == module_id && base == expected)
}

fn observed_magisk_version(result: &CmdResult, expected: &str) -> bool {
    if !result.ok || result.code != 0 || result.stdout.trim().lines().count() != 1 { return false; }
    let actual = result.stdout.trim().split(':').next().unwrap_or("");
    let expected = expected.strip_prefix('v').unwrap_or(expected);
    !expected.is_empty() && actual == expected
}

/// Only all three successful negative observations establish known-disabled state.
fn lsposed_observation_state(wanted_none: bool, root: bool, active: Option<bool>, staged: Option<bool>,
    lspd: Option<bool>, selected: Option<bool>) -> (bool, bool, &'static str) {
    if wanted_none {
        if root && active == Some(false) && staged == Some(false) && lspd == Some(false) {
            return (true, false, "disabled");
        }
        if active == Some(true) || staged == Some(true) || lspd == Some(true) {
            return (false, false, "disable-pending-restart");
        }
        return (false, false, "unknown");
    }
    if !root || active.is_none() || staged.is_none() || lspd.is_none() {
        return (false, false, "unknown");
    }
    match selected {
        Some(false) => (false, false, "not-installed"),
        Some(true) => (false, true, match lspd {
            Some(false) => "installed-not-running",
            Some(true) => "running-unverified",
            None => "unknown",
        }),
        None => (false, false, "unknown"),
    }
}

pub async fn verify_vm(adb: &Adb, c: &VmConfig) -> Result<VerifyVmResult> {
    c.validate()?;
    let expected_magisk = match c.root_manager.as_str() {
        "none" => None,
        selection => Some(selection.strip_prefix("magisk@")
            .ok_or_else(|| Error::Other("验证仅支持none或Magisk选择".into()))?),
    };
    let boot_id = crate::guest::current_boot(adb).await?;
    let daemon = adb.shell(&process_probe("magiskd")).await.ok();
    let magisk_present = daemon.as_ref().and_then(observed_process);
    // A stock/none observation must never issue su just to inspect private dirs.
    let root = if expected_magisk.is_some() { adb.check_root().await } else { false };
    let root_verified = if let Some(expected) = expected_magisk {
        if root {
            let actual = adb.su_shell(&format!("{} -v", c.magisk_path)).await.ok();
            magisk_present == Some(true) && actual.as_ref().map(|result| observed_magisk_version(result, expected)).unwrap_or(false)
        } else { false }
    } else { magisk_present == Some(false) };
    let process = adb.shell(&process_probe("lspd")).await.ok();
    let lspd = process.as_ref().and_then(observed_process);
    let wanted = c.lsp_desired();
    let (active, staged) = if root {
        let active = adb.su_shell(&modules_probe(false)).await.ok();
        let staged = adb.su_shell(&modules_probe(true)).await.ok();
        (active.as_ref().and_then(observed_modules), staged.as_ref().and_then(observed_modules))
    } else { (None, None) };
    let version = wanted.rsplit_once('-').map(|(v, _)| v).unwrap_or(wanted);
    let expected_id = if wanted.ends_with("-zygisk") { Some("zygisk_lsposed") }
        else if wanted.ends_with("-riru") { Some("riru_lsposed") } else { None };
    let selected = if root {
        if let Some(module_id) = expected_id {
            let result = adb.su_shell(&selected_module_probe(module_id)).await.ok();
            result.as_ref().and_then(|result| observed_selected_module(result, module_id, version))
        } else { None }
    } else { None };
    let (lsp_verified, lsp_installed, lsp_state) = lsposed_observation_state(wanted == "none", root, active, staged, lspd, selected);
    crate::guest::require_same_boot(&boot_id, &crate::guest::current_boot(adb).await?)?;
    Ok(VerifyVmResult { root_verified, lsp_verified, lsp_installed, lsp_state: lsp_state.into(),
        pending_restart: false, boot_id,
        message: "unknown表示探测未完成、失败或证据不足，不代表已禁用；进程和模块文件仅作部分证据，LSPosed完整激活须管理器复核；准备ramdisk须保留官方backup和PREINITDEVICE".into() })
}

#[cfg(test)]
mod reliable_verification_tests {
    use super::*;

    fn observation(ok: bool, code: i32, stdout: &str) -> CmdResult {
        CmdResult { ok, code, stdout: stdout.into(), stderr: String::new() }
    }

    #[test]
    fn tagged_modules_require_success_exit_zero_and_one_exact_marker() {
        assert_eq!(observed_modules(&observation(true, 0, "ROOST_MODULES_DISABLED\n")), Some(false));
        assert_eq!(observed_modules(&observation(true, 0, "ROOST_MODULES_ENABLED\n")), Some(true));
        for text in ["", "STAGED_ENABLED", "prefix ROOST_MODULES_DISABLED", "ROOST_MODULES_DISABLED\nROOST_MODULES_ENABLED", "ROOST_MODULES_DISABLED\nROOST_MODULES_DISABLED"] {
            assert_eq!(observed_modules(&observation(true, 0, text)), None);
        }
        assert_eq!(observed_modules(&observation(false, 0, "ROOST_MODULES_DISABLED")), None);
        assert_eq!(observed_modules(&observation(true, 1, "ROOST_MODULES_DISABLED")), None);
        assert_eq!(observed_process(&observation(true, 1, "ROOST_PROCESS_ABSENT")), None);
    }

    #[test]
    fn known_disabled_requires_root_and_all_negative_successful_observations() {
        assert_eq!(lsposed_observation_state(true, true, Some(false), Some(false), Some(false), None), (true, false, "disabled"));
        for (root, active, staged, lspd) in [
            (false, None, None, Some(false)), (false, Some(false), Some(false), Some(false)),
            (true, None, Some(false), Some(false)), (true, Some(false), None, Some(false)),
            (true, Some(false), Some(false), None),
        ] {
            assert_eq!(lsposed_observation_state(true, root, active, staged, lspd, None), (false, false, "unknown"));
        }
        for (active, staged, lspd) in [(Some(true), Some(false), Some(false)),
            (Some(false), Some(true), Some(false)), (None, None, Some(true))] {
            assert_eq!(lsposed_observation_state(true, true, active, staged, lspd, None), (false, false, "disable-pending-restart"));
        }
    }

    #[test]
    fn selected_module_requires_exact_unique_id_version_and_successful_envelope() {
        let selected = "ROOST_SELECTED_BEGIN\nid=zygisk_lsposed\nversion=v1.9.2\nversionCode=7024\nROOST_SELECTED_END\n";
        assert_eq!(observed_selected_module(&observation(true, 0, selected), "zygisk_lsposed", "1.9.2"), Some(true));
        assert_eq!(observed_selected_module(&observation(true, 0, selected), "riru_lsposed", "1.9.2"), Some(false));
        assert_eq!(observed_selected_module(&observation(true, 0, selected), "zygisk_lsposed", "1.9.20"), Some(false));
        assert_eq!(observed_selected_module(&observation(true, 1, selected), "zygisk_lsposed", "1.9.2"), None);
        assert_eq!(observed_selected_module(&observation(false, 0, selected), "zygisk_lsposed", "1.9.2"), None);
        assert_eq!(observed_selected_module(&observation(true, 0, "ROOST_SELECTED_ABSENT\n"), "zygisk_lsposed", "1.9.2"), Some(false));
        for text in ["id=zygisk_lsposed\nversion=1.9.2", "ROOST_SELECTED_BEGIN\nid=zygisk_lsposed\nid=riru_lsposed\nversion=1.9.2\nROOST_SELECTED_END",
            "ROOST_SELECTED_BEGIN\nid=zygisk_lsposed\nversion=1.9.2\nversion=1.9.2\nROOST_SELECTED_END",
            "ROOST_SELECTED_BEGIN\nid=zygisk_lsposed\nversion=1.9.2 test\nROOST_SELECTED_END\nError: failed"] {
            assert_eq!(observed_selected_module(&observation(true, 0, text), "zygisk_lsposed", "1.9.2"), None);
        }
    }

    #[test]
    fn real_lsposed_release_version_accepts_only_exact_matching_decimal_build() {
        // Literal fields read from LSPosed-v1.9.2-7024-zygisk-release.zip module.prop.
        let fixture = "ROOST_SELECTED_BEGIN\nid=zygisk_lsposed\nname=Zygisk - LSPosed\nversion=v1.9.2 (7024)\nversionCode=7024\nROOST_SELECTED_END\n";
        assert_eq!(observed_selected_module(&observation(true, 0, fixture), "zygisk_lsposed", "1.9.2"), Some(true));
        assert_eq!(observed_selected_module(&observation(true, 0, fixture), "zygisk_lsposed", "v1.9.2"), Some(true));
        assert_eq!(observed_selected_module(&observation(true, 0, fixture), "zygisk_lsposed", "1.9.20"), Some(false));
        for (actual, codes) in [("v1.9.2 (7024)", "versionCode=7025\n"), ("v1.9.2 (7024)", ""),
            ("v1.9.2 (7024)", "versionCode=7024\nversionCode=7024\n"),
            ("v1.9.2", "versionCode=7024\nversionCode=7025\n"),
            ("v1.9.2 (7024)", "versionCode=07024\n"), ("v1.9.2 (7024)", "versionCode=7024x\n"),
            ("v1.9.2  (7024)", "versionCode=7024\n"), ("v1.9.2\t(7024)", "versionCode=7024\n"),
            ("v1.9.2 (7024) ", "versionCode=7024\n"), ("v1.9.2 (7024) junk", "versionCode=7024\n"),
            ("v1.9.2 test", "versionCode=7024\n"), ("v1.9.2 ()", "versionCode=7024\n"),
            ("v1.9.2 (+7024)", "versionCode=7024\n"), ("v1.9.2 (７０２４)", "versionCode=7024\n")] {
            let text = format!("ROOST_SELECTED_BEGIN\nid=zygisk_lsposed\nversion={actual}\n{codes}ROOST_SELECTED_END\n");
            assert_eq!(observed_selected_module(&observation(true, 0, &text), "zygisk_lsposed", "1.9.2"), None, "{text}");
        }
        for actual in ["v1.9.20", "v1.9.20 (7024)", "vv1.9.2", "v1.9.2(7024)"] {
            let text = format!("ROOST_SELECTED_BEGIN\nid=zygisk_lsposed\nversion={actual}\nversionCode=7024\nROOST_SELECTED_END\n");
            assert_eq!(observed_selected_module(&observation(true, 0, &text), "zygisk_lsposed", "1.9.2"), Some(false), "{text}");
        }
        let no_suffix = "ROOST_SELECTED_BEGIN\nid=zygisk_lsposed\nversion=v1.9.2\nROOST_SELECTED_END";
        assert_eq!(observed_selected_module(&observation(true, 0, no_suffix), "zygisk_lsposed", "1.9.2"), Some(true));
    }

    #[test]
    fn verification_result_serializes_same_boot_identity_with_existing_fields() {
        let boot_id = "12345678-1234-4abc-8def-123456789abc";
        let value = serde_json::to_value(VerifyVmResult {
            root_verified: false, lsp_verified: false, lsp_installed: false,
            lsp_state: "unknown".into(), pending_restart: true, message: "pure fixture".into(), boot_id: boot_id.into(),
        }).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 7);
        assert_eq!(value["boot_id"], boot_id);
        assert_eq!(value["root_verified"], false);
        assert_eq!(value["lsp_verified"], false);
        assert_eq!(value["lsp_installed"], false);
        assert_eq!(value["lsp_state"], "unknown");
        assert_eq!(value["pending_restart"], true);
        assert_eq!(value["message"], "pure fixture");
    }

    #[test]
    fn installed_or_process_present_never_proves_framework_activation() {
        for (lspd, expected) in [(Some(true), "running-unverified"), (Some(false), "installed-not-running")] {
            assert_eq!(lsposed_observation_state(false, true, Some(true), Some(false), lspd, Some(true)), (false, true, expected));
        }
        for (root, active, staged, lspd, selected) in [(false, None, None, Some(true), None),
            (true, Some(true), None, Some(true), Some(true)), (true, None, Some(false), Some(true), Some(true)),
            (true, Some(true), Some(false), None, Some(true))] {
            assert_eq!(lsposed_observation_state(false, root, active, staged, lspd, selected), (false, false, "unknown"));
        }
        assert_eq!(lsposed_observation_state(false, true, Some(false), Some(false), Some(false), Some(false)), (false, false, "not-installed"));
    }

    #[test]
    fn magisk_version_requires_exact_single_line_and_success_exit_code() {
        assert!(observed_magisk_version(&observation(true, 0, "28.0:MAGISK:R\n"), "v28.0"));
        for (ok, code, output) in [(false, 0, "28.0:MAGISK:R"), (true, 1, "28.0:MAGISK:R"),
            (true, 0, "28.00:MAGISK:R"), (true, 0, "28.0:MAGISK:R\nError: failed"), (true, 0, "")] {
            assert!(!observed_magisk_version(&observation(ok, code, output), "28.0"));
        }
    }
}

/// Explicit setup action; source APK/resources are versioned and copied only to this VM.
pub async fn initialize_magisk(adb: &Adb, c: &VmConfig) -> Result<CmdResult> {
    if !c.root_manager.starts_with("magisk@") || !adb.check_root().await {
        return Err(Error::Other("请先使用准备好的Magisk ramdisk启动，并在Magisk管理器中明确授权Shell；本操作不自动授权策略或执行adb root".into()));
    }
    let version = c.root_manager.trim_start_matches("magisk@");
    validate_component(version)?;
    let abi = adb.getprop("ro.product.cpu.abi").await;
    if abi != "x86_64" && abi != "arm64-v8a" { return Err(Error::Other(format!("暂未支持该ABI资源部署: {abi}"))); }
    let package_dir = crate::roots::root_binaries_dir().join("magisk").join(version);
    let expected = std::fs::read_to_string(package_dir.join("package.sha256"))?;
    let digest = sha256_file(&package_dir.join("package.apk"))?;
    if expected.split_whitespace().next() != Some(digest.as_str()) {
        return Err(Error::Other("缓存Magisk APK的SHA256记录不匹配；请重新下载".into()));
    }
    let resources = package_dir.join("resources");
    let mut files: Vec<(PathBuf, &str)> = vec![
        (resources.join("assets/util_functions.sh"), "util_functions.sh"),
        (resources.join("assets/boot_patch.sh"), "boot_patch.sh"),
        (resources.join("assets/addon.d.sh"), "addon.d.sh"),
        (resources.join("assets/module_installer.sh"), "module_installer.sh"),
        (resources.join("assets/app_functions.sh"), "app_functions.sh"),
    ];
    for (lib, target) in [("libbusybox.so", "busybox"), ("libmagiskboot.so", "magiskboot"),
        ("libmagiskinit.so", "magiskinit"), ("libmagiskpolicy.so", "magiskpolicy")] {
        files.push((resources.join("lib").join(&abi).join(lib), target));
    }
    let init_ld = resources.join("lib").join(&abi).join("libinit-ld.so");
    if init_ld.is_file() { files.push((init_ld, "init-ld")); }
    for (source, _) in &files {
        if !source.is_file() { return Err(Error::Other(format!("对应版本完整资源未准备，请重新下载Magisk {version}: {}", source.display()))); }
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let staging = format!("/data/local/tmp/roost-magisk-{}-{stamp}", c.id);
    require_ok(adb.shell(&format!("mkdir -p {staging}")).await?)?;
    let result: Result<CmdResult> = async {
        for (source, target) in &files {
            require_ok(adb.push(&source.to_string_lossy(), &format!("{staging}/{target}")).await?)?;
        }
        require_ok(adb.su_shell("mkdir -p /data/adb/magisk").await?)?;
        for (_, target) in &files {
            require_ok(adb.su_shell(&format!("cp {staging}/{target} /data/adb/magisk/{target} && chmod 755 /data/adb/magisk/{target}")).await?)?;
        }
        require_ok(adb.su_shell(&format!("cp {} /data/adb/magisk/magisk && chmod 755 /data/adb/magisk/magisk && test -x /data/adb/magisk/busybox", c.magisk_path)).await?)
    }.await;
    let _ = adb.shell(&format!("rm -rf {staging}")).await;
    result
}

pub fn lsposed_package(selection: &str) -> Result<PathBuf> {
    validate_component(selection)?;
    // Automatic baseline is deliberately limited to the public, unlicensed 1.9.2 packages.
    if selection != "1.9.2-zygisk" && selection != "1.9.2-riru" {
        return Err(Error::Other("自动应用基线仅支持公开 LSPosed 1.9.2；其他包请先检查来源/兼容性后手动安装".into()));
    }
    let dir = crate::packages::lsposed_dir().join(selection);
    let mut zips: Vec<PathBuf> = std::fs::read_dir(&dir)?.filter_map(|v| v.ok().map(|e| e.path()))
        .filter(|v| v.extension().and_then(|s| s.to_str()) == Some("zip")).collect();
    zips.sort();
    if zips.len() != 1 { return Err(Error::Other("LSPosed 缓存须恰好包含一个 ZIP".into())); }
    Ok(zips.remove(0))
}
