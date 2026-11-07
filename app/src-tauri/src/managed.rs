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

pub fn prepare_emulator_data(c: &VmConfig) -> Result<()> {
    c.validate()?;
    validate_component(&c.avd_name)?;
    for value in [&c.sdk_path, &c.avd_home, &c.emulator_image_dir] {
        if !Path::new(value).is_absolute() { return Err(Error::Other("SDK/源AVD/镜像目录必须配置绝对路径".into())); }
    }
    let image = PathBuf::from(&c.emulator_image_dir);
    for name in ["system.img", "ramdisk.img", "userdata.img"] {
        if !image.join(name).is_file() { return Err(Error::PathNotFound(image.join(name))); }
    }
    // Require an explicitly selected x86_64 image for the accelerated backend.
    let prop = std::fs::read_to_string(image.join("build.prop")).unwrap_or_default();
    if !prop.lines().any(|l| l.trim() == "ro.product.cpu.abi=x86_64" || l.trim() == "ro.system.product.cpu.abi=x86_64") {
        return Err(Error::Other("Emulator 镜像 build.prop 未确认 x86_64 ABI".into()));
    }
    // Copy only the selected AVD metadata; all mutable paths stay under this VM.
    let source_avd = PathBuf::from(&c.avd_home).join(format!("{}.avd", c.avd_name)).join("config.ini");
    if !source_avd.is_file() { return Err(Error::PathNotFound(source_avd)); }
    let source_config = std::fs::read_to_string(&source_avd)?;
    if !source_config.lines().any(|line| line.trim() == "abi.type=x86_64") {
        return Err(Error::Other("源AVD config.ini未确认abi.type=x86_64".into()));
    }
    let avd_dir = c.dir().join("avd").join(format!("{}.avd", c.avd_name));
    std::fs::create_dir_all(&avd_dir)?;
    let mut config = std::fs::read_to_string(&source_avd)?.lines()
        .filter(|l| {
            let key = l.split('=').next().unwrap_or("").trim();
            !key.starts_with("disk.") && !key.starts_with("snapshot.") && !key.starts_with("sdcard.")
                && !key.starts_with("skin.") && !key.contains("path") && !key.starts_with("image.sysdir.")
                && key != "hw.sdCard" && key != "fastboot.forceChosenSnapshotBoot"
        })
        .collect::<Vec<_>>().join("\n");
    config.push_str(&format!("\nimage.sysdir.1={}\nhw.sdCard=no\nfastboot.forceColdBoot=yes\n", image.to_string_lossy().replace('\\', "/")));
    std::fs::write(avd_dir.join("config.ini"), config)?;
    std::fs::write(c.dir().join("avd").join(format!("{}.ini", c.avd_name)),
        format!("avd.ini.encoding=UTF-8\npath={}\n", avd_dir.to_string_lossy()))?;
    let data = c.dir().join("emulator");
    std::fs::create_dir_all(&data)?;
    let identity_path = data.join("image-origin.json");
    let identity = serde_json::json!({ "image": image.canonicalize()?.to_string_lossy(), "build_prop_sha256": sha256_file(&image.join("build.prop"))? });
    if identity_path.is_file() {
        let existing: serde_json::Value = serde_json::from_slice(&std::fs::read(&identity_path)?)?;
        if existing != identity { return Err(Error::Other("该VM数据盘属于另一镜像；请新建VM以避免混用userdata/加密密钥".into())); }
    }
    let userdata = data.join("userdata-qemu.img");
    if !userdata.exists() {
        std::fs::copy(image.join("userdata.img"), &userdata)?;
        // Original userdata is initialized, not the template AVD's encrypted user data.
        if image.join("encryptionkey.img").is_file() {
            std::fs::copy(image.join("encryptionkey.img"), data.join("encryptionkey.img"))?;
        }
    }
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
        "-port".into(), c.console_port.to_string(), "-accel".into(), "on".into(),
        "-memory".into(), c.memory_mb.to_string(), "-cores".into(), c.cores.to_string(),
        "-gpu".into(), "swiftshader_indirect".into(), "-no-snapshot".into()];
    if image.join("vendor.img").is_file() { args.extend(["-vendor".into(), image.join("vendor.img").to_string_lossy().into_owned()]); }
    if headless { args.extend(["-no-window".into(), "-no-audio".into()]); }
    Ok(args)
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

pub async fn install_zip(adb: &Adb, c: &VmConfig, path: &str) -> Result<ModuleInstallResult> {
    let local = Path::new(path);
    if local.extension().and_then(|s| s.to_str()).map(|s| !s.eq_ignore_ascii_case("zip")).unwrap_or(true) {
        return Err(Error::Other("Magisk 安装器仅接收 .zip 模块包".into()));
    }
    let mut archive = zip::ZipArchive::new(std::fs::File::open(local)?)
        .map_err(|e| Error::Other(format!("模块 ZIP 无效: {e}")))?;
    let mut prop = String::new();
    {
        use std::io::Read;
        let member = archive.by_name("module.prop").map_err(|_| Error::Other("ZIP 根目录缺少 module.prop".into()))?;
        if member.size() > 65536 { return Err(Error::Other("module.prop 过大".into())); }
        member.take(65537).read_to_string(&mut prop)?;
    }
    let module_id = prop.lines().find_map(|l| l.trim().strip_prefix("id=")).unwrap_or("").trim().to_string();
    validate_component(&module_id)?;
    if !module_id.chars().next().map(|v| v.is_ascii_alphabetic()).unwrap_or(false) {
        return Err(Error::Other("Magisk module id须以字母开头".into()));
    }
    drop(archive);
    if !adb.is_booted().await { return Err(Error::Other("VM 尚未完成启动".into())); }
    if !adb.check_root().await { return Err(Error::Other("VM 尚未验证 root；请先启动准备好的 Magisk ramdisk".into())); }
    require_ok(adb.su_shell(&format!("{} -v", c.magisk_path)).await?)?;
    let environment = adb.su_shell("test -x /data/adb/magisk/busybox && test -x /data/adb/magisk/magiskboot && test -f /data/adb/magisk/util_functions.sh").await?;
    if !environment.ok { return Err(Error::Other("该VM缺少Magisk运行资源；请先点击初始化Magisk运行环境（需已缓存对应完整APK）".into())); }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let remote = format!("/data/local/tmp/roost-{}-{stamp}.zip", c.id);
    require_ok(adb.push(path, &remote).await?)?;
    let result = adb.su_shell(&format!("{} --install-module {remote}", c.magisk_path)).await;
    // Cleanup is limited to this operation's generated file on this exact serial.
    let _ = adb.shell(&format!("rm -f {remote}")).await;
    let result = require_ok(result?)?;
    let state = adb.su_shell(&format!("test -f /data/adb/modules_update/{module_id}/module.prop || test -f /data/adb/modules/{module_id}/module.prop")).await?;
    if !state.ok { return Err(Error::Other(format!("安装器退出成功但未发现模块 {module_id}；{}", result.output()))); }
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
}

fn process_probe(name: &str) -> String {
    // Only fixed internal process names are accepted; never interpolate user input.
    debug_assert!(name == "magiskd" || name == "lspd");
    format!(r#"command -v pidof >/dev/null 2>&1 || exit 127; pids=$(pidof {name}); code=$?; if [ "$code" -eq 0 ] && [ -n "$pids" ]; then printf 'ROOST_PROCESS_PRESENT\n'; elif [ "$code" -eq 1 ]; then printf 'ROOST_PROCESS_ABSENT\n'; else exit 2; fi"#)
}

fn observed_process(result: &CmdResult) -> Option<bool> {
    if !result.ok { return None; }
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

pub async fn verify_vm(adb: &Adb, c: &VmConfig) -> Result<VerifyVmResult> {
    if !adb.is_booted().await { return Err(Error::Other("VM 尚未完成启动".into())); }
    let root = adb.check_root().await;
    let daemon = adb.shell(&process_probe("magiskd")).await?;
    let magisk_present = observed_process(&daemon)
        .ok_or_else(|| Error::Other(format!("Magisk进程观测失败，未判定为无Root: {}", daemon.output())))?;
    let root_verified = if c.root_manager == "none" { !magisk_present } else {
        let actual = adb.su_shell(&format!("{} -v", c.magisk_path)).await?;
        let expected = c.root_manager.strip_prefix("magisk@").unwrap_or("");
        actual.ok && root && actual.stdout.trim().split(':').next().unwrap_or("") == expected.trim_start_matches('v')
    };
    let check = adb.su_shell(r#"for d in /data/adb/modules/zygisk_lsposed /data/adb/modules/riru_lsposed; do if [ -d "$d" ] && [ ! -e "$d/disable" ] && [ ! -e "$d/remove" ]; then cat "$d/module.prop"; fi; done; pidof lspd; true"#).await?;
    let wanted = c.lsp_desired();
    let staged = adb.su_shell(r#"for d in /data/adb/modules_update/zygisk_lsposed /data/adb/modules_update/riru_lsposed; do if [ -d "$d" ] && [ ! -e "$d/disable" ] && [ ! -e "$d/remove" ]; then echo STAGED_ENABLED; fi; done"#).await?;
    let staged_enabled = staged.ok && staged.stdout.contains("STAGED_ENABLED");
    let process = adb.shell(&process_probe("lspd")).await?;
    let lspd = observed_process(&process)
        .ok_or_else(|| Error::Other(format!("LSPosed进程观测失败，未判定为已禁用: {}", process.output())))?;
    let version = wanted.rsplit_once('-').map(|(v,_)| v).unwrap_or(wanted);
    let expected_id = if wanted.ends_with("-zygisk") { Some("zygisk_lsposed") }
        else if wanted.ends_with("-riru") { Some("riru_lsposed") } else { None };
    let lsp_installed = if let Some(module_id) = expected_id {
        let selected = adb.su_shell(&format!("d=/data/adb/modules/{module_id}; test -d \"$d\" && test ! -e \"$d/disable\" && test ! -e \"$d/remove\" && cat \"$d/module.prop\"")).await?;
        let has_id = selected.stdout.lines().any(|line| line.trim() == format!("id={module_id}"));
        selected.ok && has_id && selected.stdout.lines().filter_map(|line| line.trim().strip_prefix("version="))
            .any(|value| value.split_whitespace().next().unwrap_or("").trim_start_matches('v') == version)
    } else { false };
    let (lsp_verified, lsp_state) = if wanted == "none" {
        (!lspd && !staged_enabled && (!root || (check.ok && !check.stdout.contains("id="))),
            if lspd || staged_enabled { "disable-pending-restart".to_string() } else { "disabled".to_string() })
    } else if !lsp_installed { (false, "not-installed".into()) }
    else if !lspd { (false, "installed-not-running".into()) }
    else {
        let preinit = adb.su_shell("test -s /metadata/magisk/sepolicy.rule || test -s /persist/magisk/sepolicy.rule").await?;
        // A process, ZIP exit status, or rule file alone never proves framework activation.
        (false, if preinit.ok { "running-unverified: 请在LSPosed管理器确认完整激活".into() }
            else { "partial: 缺少已检查的preinit sepolicy.rule；核查准备ramdisk的PREINITDEVICE".into() })
    };
    Ok(VerifyVmResult { root_verified, lsp_verified, lsp_installed, lsp_state,
        pending_restart: false,
        message: "LSPosed进程/文件仅作部分状态证据，完整激活须管理器复核；准备ramdisk须保留官方backup和PREINITDEVICE".into() })
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
