
//! Windowless, single-VM JSONL control. No Tauri runtime or arbitrary commands.
//!
//! Start with `--headless-control <vm-id>` and pipe stdin/stdout. Requests are
//! `{ "id": 1, "op": "launch" }`. Unknown fields/operations are fatal. EOF,
//! shutdown, input/output errors and command failures all clean up only the
//! VmManager created by this session. Existing external VMs are never adopted.

use crate::adb::Adb;
use crate::types::{validate_component, Error, Result, VmConfig};
use crate::vm::VmManager;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const PROTOCOL: &str = "roost-headless-v1";
const MAX_REQUEST_BYTES: usize = 16 * 1024;
const OPERATIONS: [&str; 8] = [
    "launch", "stop", "status", "verify", "initialize_magisk", "apply_lsposed", "capture", "shutdown",
];

#[derive(Debug, PartialEq)]
enum Entry {
    Gui,
    Control(String),
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> std::result::Result<Entry, String> {
    let args: Vec<OsString> = args.into_iter().collect();
    if !args.iter().any(|arg| arg.to_string_lossy().starts_with("--headless-control")) {
        return Ok(Entry::Gui);
    }
    if args.len() != 2 || args[0] != "--headless-control" {
        return Err("Usage: --headless-control <vm-id>; no additional arguments".into());
    }
    let vm_id = args[1].to_str().ok_or("VM id must be UTF-8")?;
    validate_component(vm_id).map_err(|error| error.to_string())?;
    Ok(Entry::Control(vm_id.to_string()))
}

/// Returns None for the existing GUI entry; Some always bypasses Tauri.
/// The binary uses the Windows GUI subsystem in debug and release, and callers
/// supply redirected standard handles rather than allocating/attaching a console.
pub fn dispatch(args: impl IntoIterator<Item = OsString>) -> Option<i32> {
    let entry = match parse_args(args) {
        Ok(Entry::Gui) => return None,
        Ok(Entry::Control(vm_id)) => vm_id,
        Err(error) => {
            startup_error(&error);
            return Some(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            startup_error(&format!("Control runtime: {error}"));
            return Some(1);
        }
    };
    let mut session = match Session::new(&entry) {
        Ok(session) => session,
        Err(error) => {
            startup_error(&error.to_string());
            return Some(1);
        }
    };
    let stdin = io::stdin();
    let stdout = io::stdout();
    // Read the whole request stream up front. Launching the emulator spawns adb children
    // that can disturb the inherited stdin handle on Windows, after which a live read
    // returned an empty buffer and the next frame failed to parse. Buffering here keeps
    // the protocol independent of any child process behaviour.
    let mut pending = Vec::new();
    if let Err(error) = io::Read::read_to_end(&mut stdin.lock(), &mut pending) {
        startup_error(&format!("stdin: {error}"));
        return Some(1);
    }
    let mut reader = io::Cursor::new(pending);
    Some(runtime.block_on(session.run(&mut reader, &mut stdout.lock())))
}

fn startup_error(message: &str) {
    let _ = write_json(&mut io::stdout().lock(), &json!({
        "protocol": PROTOCOL, "event": "fatal", "ok": false, "error": message,
    }));
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Launch,
    Stop,
    Status,
    Verify,
    InitializeMagisk,
    ApplyLsposed,
    Capture,
    Shutdown,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    #[serde(default)]
    id: Value,
    op: Operation,
}

fn parse_request(bytes: &[u8]) -> Result<Request> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(Error::Other("JSONL request exceeds 16KiB".into()));
    }
    let request: Request = serde_json::from_slice(bytes)?;
    if !matches!(request.id, Value::Null | Value::String(_) | Value::Number(_)) {
        return Err(Error::Other("Request id must be a string, number or null".into()));
    }
    Ok(request)
}

/// BufRead::lines may allocate an unbounded line before returning. Bound first.
/// Blank lines are skipped: a caller that writes several requests at once can leave
/// stray separators, and an empty frame must never be parsed as JSON.
fn read_request_line(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let (count, ended) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return if line.is_empty() { Ok(None) } else { Ok(Some(line)) };
            }
            let count = available.iter().position(|byte| *byte == b'\n')
                .map(|index| index + 1).unwrap_or(available.len());
            if line.len() + count > MAX_REQUEST_BYTES {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "JSONL request exceeds 16KiB"));
            }
            line.extend_from_slice(&available[..count]);
            (count, available[count - 1] == b'\n')
        };
        reader.consume(count);
        if ended {
            line.pop();
            if line.last() == Some(&b'\r') { line.pop(); }
            if line.is_empty() { continue; }
            return Ok(Some(line));
        }
    }
}

fn write_json(writer: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

struct Session {
    vm_id: String,
    serial: String,
    vm_dir: PathBuf,
    manager: VmManager,
    capture_number: u64,
    evidence_prefix: String,
}

impl Session {
    fn new(vm_id: &str) -> Result<Self> {
        validate_component(vm_id)?;
        let config = VmConfig::load(vm_id)?;
        config.validate()?;
        let serial = config.serial()?;
        let vm_dir = validate_vm_evidence_dir(&config.dir())?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)
            .map_err(|error| Error::Other(error.to_string()))?.as_nanos();
        Ok(Self {
            vm_id: vm_id.into(), serial, vm_dir, manager: VmManager::new(), capture_number: 0,
            evidence_prefix: format!("guest-{}-{stamp}", std::process::id()),
        })
    }

    fn owns_vm(&self) -> bool {
        // This private manager is never shared or populated from persisted PIDs.
        self.manager.running_config(&self.vm_id).is_ok()
    }

    fn bound_config(&self) -> Result<VmConfig> {
        let config = VmConfig::load(&self.vm_id)?;
        require_serial(&config, &self.serial)?;
        reject_reparse_ancestors(&config.dir())?;
        if fs::canonicalize(config.dir())? != self.vm_dir {
            return Err(Error::Other("VM directory changed during control session".into()));
        }
        Ok(config)
    }

    fn owned_adb(&self) -> Result<(VmConfig, Adb)> {
        if !self.owns_vm() {
            return Err(Error::Other("Operation requires a VM launched by this session".into()));
        }
        let running = self.manager.running_config(&self.vm_id)?;
        require_serial(&running, &self.serial)?;
        Ok((running, self.manager.adb_for_vm(&self.vm_id)?))
    }

    async fn cleanup(&self) -> Result<()> {
        // Also covers launch returning Err after recording a retained child/job.
        if self.owns_vm() { self.manager.stop(&self.vm_id).await?; }
        Ok(())
    }

    async fn run(&mut self, reader: &mut impl BufRead, writer: &mut impl Write) -> i32 {
        let ready = json!({ "protocol": PROTOCOL, "event": "ready", "ok": true,
            "vm_id": self.vm_id, "serial": self.serial, "owned": false, "operations": OPERATIONS });
        let outcome = match write_json(writer, &ready) {
            Ok(()) => self.request_loop(reader, writer).await,
            Err(error) => Err(error),
        };
        // Run cleanup before reporting fatal errors; an unavailable stdout must
        // never prevent release of the private manager's VM process tree.
        let cleanup = self.cleanup().await;
        if let Err(error) = &outcome {
            let _ = write_json(writer, &json!({ "protocol": PROTOCOL, "event": "fatal", "ok": false,
                "vm_id": self.vm_id, "serial": self.serial, "error": error.to_string() }));
        }
        let clean = cleanup.is_ok();
        let closed = write_json(writer, &json!({ "protocol": PROTOCOL, "event": "closed", "ok": outcome.is_ok() && clean,
            "cleanup_ok": clean, "vm_id": self.vm_id, "serial": self.serial, "owned": self.owns_vm(),
            "error": cleanup.err().map(|error| error.to_string()) }));
        if outcome.is_ok() && clean && closed.is_ok() { 0 } else { 1 }
    }

    async fn request_loop(&mut self, reader: &mut impl BufRead, writer: &mut impl Write) -> Result<()> {
        while let Some(line) = read_request_line(reader)? {
            let request = parse_request(&line)?;
            let shutdown = request.op == Operation::Shutdown;
            match self.execute(request.op).await {
                Ok(result) => write_json(writer, &json!({ "protocol": PROTOCOL, "id": request.id,
                    "vm_id": self.vm_id, "serial": self.serial, "ok": true, "owned": self.owns_vm(),
                    "result": result }))?,
                Err(error) => {
                    // Correlate the failed request, then terminate and clean up.
                    write_json(writer, &json!({ "protocol": PROTOCOL, "id": request.id,
                        "vm_id": self.vm_id, "serial": self.serial, "ok": false,
                        "error": error.to_string(), "owned": self.owns_vm() }))?;
                    return Err(error);
                }
            }
            if shutdown { return Ok(()); }
        }
        Ok(())
    }

    async fn execute(&mut self, operation: Operation) -> Result<Value> {
        // Stop/shutdown always use the manager's captured config, even if the
        // on-disk file was subsequently edited or removed.
        match operation {
            Operation::Shutdown => {
                self.cleanup().await?;
                return Ok(json!({ "shutdown": true }));
            }
            Operation::Stop => {
                if !self.owns_vm() { return Ok(json!({ "stopped": false, "reason": "not_owned" })); }
                let message = self.manager.stop(&self.vm_id).await?;
                return Ok(json!({ "stopped": true, "message": message }));
            }
            Operation::Status => {
                let running = self.manager.is_running(&self.vm_id);
                let owned = self.owns_vm();
                // Poll only the exact serial held by this manager. ADB offline,
                // timeout or lost ownership is an observation, not a fatal error.
                let boot_completed = if running && owned {
                    match self.owned_adb() {
                        Ok((_, adb)) => Some(adb.is_booted().await),
                        Err(_) => None,
                    }
                } else { None };
                return Ok(status_payload(running, owned, boot_completed));
            }
            _ => {}
        }
        let desired = self.bound_config()?;
        if operation == Operation::Launch {
            let result = self.manager.launch_captured(desired, true).await?;
            if !result.ok { return Err(Error::Other(result.message)); }
            if result.serial != self.serial {
                return Err(Error::Other("Launch serial differs from session binding".into()));
            }
            return Ok(serde_json::to_value(result)?);
        }
        let _lease = self.manager.operation(&self.vm_id)?;
        let (running, adb) = self.owned_adb()?;
        match operation {
            Operation::Verify => {
                let mut expected = running;
                expected.lsp_version = desired.lsp_version;
                // Observation only: avoid independently rewriting GUI state logic.
                let result = crate::managed::verify_vm(&adb, &expected).await?;
                let mut observation = serde_json::to_value(result)?;
                observation["pending_restart"] = Value::Null;
                Ok(json!({ "verification": observation, "configuration_updated": false,
                    "pending_restart_reconciled": false }))
            }
            Operation::InitializeMagisk => {
                let result = crate::managed::initialize_magisk(&adb, &running).await?;
                if !result.ok { return Err(Error::Command(result.output())); }
                Ok(serde_json::to_value(result)?)
            }
            Operation::ApplyLsposed => apply_lsposed(&adb, &running, desired).await,
            Operation::Capture => self.capture(&adb).await,
            _ => Err(Error::Other("Unexpected control operation".into())),
        }
    }

    async fn capture(&mut self, adb: &Adb) -> Result<Value> {
        let png = adb.capture_png().await?;
        self.capture_number = self.capture_number.checked_add(1)
            .ok_or_else(|| Error::Other("Capture sequence exhausted".into()))?;
        let name = format!("{}-{:06}.png", self.evidence_prefix, self.capture_number);
        let (path, mut file) = create_evidence_file(&self.vm_dir, &name)?;
        file.write_all(&png)?;
        file.sync_all()?;
        Ok(json!({ "path": path, "bytes": png.len(), "sha256": format!("{:x}", Sha256::digest(&png)),
            "source": "adb exec-out screencap -p", "serial": self.serial, "guest_only": true }))
    }
}

fn status_payload(running: bool, owned: bool, boot_completed: Option<bool>) -> Value {
    // An observation without live session ownership is never guest evidence.
    let boot_completed = if running && owned { boot_completed } else { None };
    json!({ "running": running, "scope": "session_owned_only", "external_state": "not_observed",
        "owned": owned, "boot_completed": boot_completed })
}

fn require_serial(config: &VmConfig, serial: &str) -> Result<()> {
    if config.serial()? != serial {
        return Err(Error::Other("VM serial changed during control session".into()));
    }
    Ok(())
}

/// Same managed primitives and explicit checks as the existing GUI operation.
/// No adb root, automatic su grant, reboot or arbitrary module path is exposed.
async fn apply_lsposed(adb: &Adb, running: &VmConfig, desired: VmConfig) -> Result<Value> {
    if desired.root_manager != running.root_manager || !running.root_manager.starts_with("magisk@") {
        return Err(Error::Other("Launch this VM with the selected Magisk version first".into()));
    }
    if !adb.is_booted().await || !adb.check_root().await {
        return Err(Error::Other("VM boot or explicit Shell su authorization is pending".into()));
    }
    let boot_id = crate::guest::current_boot(adb).await?;
    let result = if desired.lsp_desired() == "none" {
        crate::managed::disable_lsposed(adb).await?
    } else {
        if desired.lsp_desired().ends_with("-riru") {
            return Err(Error::Other("Automatic baseline supports the existing Zygisk package only".into()));
        }
        let zygisk = adb.su_shell(&format!(r#"{} --sqlite 'SELECT value FROM settings WHERE key="zygisk"'"#, desired.magisk_path)).await?;
        if !zygisk.ok || zygisk.code != 0 || zygisk.stdout.trim() != "value=1" {
            return Err(Error::Other("Enable Zygisk explicitly and restart the VM before applying LSPosed".into()));
        }
        let path = crate::managed::lsposed_package(desired.lsp_desired())?;
        crate::managed::install_lsposed_zip(adb, running, &path.to_string_lossy(), desired.lsp_desired()).await?
    };
    if !result.ok { return Err(Error::Other("LSPosed apply did not succeed".into())); }
    crate::guest::require_same_boot(&boot_id, &crate::guest::current_boot(adb).await?)?;
    let mut saved = desired;
    saved.pending_restart = true;
    saved.module_pending_boot_id = Some(boot_id);
    saved.save()?;
    Ok(serde_json::to_value(result)?)
}

fn reject_reparse_ancestors(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        let mut redirected = metadata.file_type().is_symlink();
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            redirected |= metadata.file_attributes() & 0x400 != 0;
        }
        if redirected {
            return Err(Error::Other("Evidence path contains a symlink or reparse point".into()));
        }
    }
    Ok(())
}

fn validate_vm_evidence_dir(vm_dir: &Path) -> Result<PathBuf> {
    reject_reparse_ancestors(vm_dir)?;
    let vm_dir = fs::canonicalize(vm_dir)?;
    if !vm_dir.is_dir() {
        return Err(Error::Other("VM path is not a directory".into()));
    }
    let evidence_dir = vm_dir.join("evidence");
    match fs::symlink_metadata(&evidence_dir) {
        Ok(metadata) => {
            reject_reparse_ancestors(&evidence_dir)?;
            if !metadata.is_dir() || fs::canonicalize(&evidence_dir)?.parent() != Some(vm_dir.as_path()) {
                return Err(Error::Other("Evidence path is not the fixed VM evidence directory".into()));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(vm_dir)
}

fn create_evidence_file(vm_dir: &Path, name: &str) -> Result<(PathBuf, File)> {
    // Defense in depth: production names are generated, not read from JSONL.
    if !name.starts_with("guest-") || !name.ends_with(".png")
        || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)) {
        return Err(Error::Other("Invalid generated evidence filename".into()));
    }
    reject_reparse_ancestors(vm_dir)?;
    let evidence_dir = vm_dir.join("evidence");
    match fs::create_dir(&evidence_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    reject_reparse_ancestors(&evidence_dir)?;
    let evidence_dir = fs::canonicalize(evidence_dir)?;
    if evidence_dir.parent() != Some(vm_dir) {
        return Err(Error::Other("Evidence directory left the fixed VM directory".into()));
    }
    let path = evidence_dir.join(name);
    // Never overwrite a previous capture or follow a pre-created file symlink.
    let file = OpenOptions::new().write(true).create_new(true).open(&path)?;
    Ok((path, file))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(|value| OsString::from(*value)).collect()
    }

    #[test]
    fn entry_selects_control_only_with_one_fixed_vm_id() {
        assert_eq!(parse_args(args(&[])).unwrap(), Entry::Gui);
        assert_eq!(parse_args(args(&["--headless-control", "vm-test"])).unwrap(), Entry::Control("vm-test".into()));
        for bad in [vec!["--headless-control"], vec!["--headless-control=vm"],
            vec!["--headless-control", "../vm"],
            vec!["--headless-control", "vm", "--serial", "emulator-5556"],
            vec!["x", "--headless-control", "vm"]] {
            assert!(parse_args(args(&bad)).is_err());
        }
    }

    #[test]
    fn protocol_is_a_strict_operation_whitelist() {
        for operation in OPERATIONS {
            assert!(parse_request(format!(r#"{{"id":1,"op":"{operation}"}}"#).as_bytes()).is_ok());
        }
        for request in [r#"{"op":"shell","command":"id"}"#,
            r#"{"op":"capture","path":"C:/desktop.png"}"#,
            r#"{"op":"status","vm_id":"another-vm"}"#,
            r#"{"op":"status","serial":"emulator-5556"}"#,
            r#"{"op":"launch","headless":false}"#,
            r#"{"op":"capture","id":{}}"#,
            r#"{"op":"status","op":"launch"}"#] {
            assert!(parse_request(request.as_bytes()).is_err(), "{request}");
        }
    }

    #[test]
    fn line_reader_bounds_input_and_accepts_eof_and_crlf() {
        let mut input = Cursor::new(b"{\"op\":\"status\"}\r\n{\"op\":\"shutdown\"}");
        assert_eq!(parse_request(&read_request_line(&mut input).unwrap().unwrap()).unwrap().op, Operation::Status);
        assert_eq!(parse_request(&read_request_line(&mut input).unwrap().unwrap()).unwrap().op, Operation::Shutdown);
        assert!(read_request_line(&mut input).unwrap().is_none());
        assert!(read_request_line(&mut Cursor::new(vec![b'x'; MAX_REQUEST_BYTES + 1])).is_err());
        assert!(parse_request(&[0xff]).is_err());
    }

    #[test]
    fn response_is_one_json_line_and_flushes() {
        let mut output = Vec::new();
        write_json(&mut output, &json!({ "text": "a\nb", "ok": true })).unwrap();
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert!(serde_json::from_slice::<Value>(&output).unwrap()["ok"].as_bool().unwrap());
    }

    #[test]
    fn status_payload_reports_boot_only_for_owned_running_vm() {
        for running in [false, true] {
            for owned in [false, true] {
                for observed in [None, Some(false), Some(true)] {
                    let status = status_payload(running, owned, observed);
                    assert_eq!(status["scope"], "session_owned_only");
                    assert_eq!(status["owned"], owned);
                    assert_eq!(status["running"], running);
                    assert_eq!(status["external_state"], "not_observed");
                    let expected = if running && owned { json!(observed) } else { Value::Null };
                    assert_eq!(status["boot_completed"], expected);
                }
            }
        }
    }

    #[test]
    fn unowned_status_is_nonfatal_and_never_needs_vm_configuration_or_adb() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut session = Session { vm_id: "missing-configuration-fixture".into(), serial: "emulator-5554".into(),
            vm_dir: PathBuf::from("unused"), manager: VmManager::new(), capture_number: 0,
            evidence_prefix: "guest-test".into() };
        let status = runtime.block_on(session.execute(Operation::Status)).unwrap();
        assert_eq!(status["running"], false);
        assert_eq!(status["owned"], false);
        assert!(status["boot_completed"].is_null());
        assert!(!session.owns_vm());
    }

    #[test]
    fn configured_5554_serial_stays_bound() {
        let config: VmConfig = serde_json::from_value(json!({ "id": "fixture", "name": "fixture",
            "backend": "android-emulator", "console_port": 5554 })).unwrap();
        assert_eq!(config.serial().unwrap(), "emulator-5554");
        assert!(require_serial(&config, "emulator-5554").is_ok());
        assert!(require_serial(&config, "emulator-5556").is_err());
    }

    #[test]
    fn foreign_vm_has_no_handle_and_stop_does_not_touch_adb() {
        let session = Session { vm_id: "foreign-fixture".into(), serial: "emulator-5554".into(),
            vm_dir: PathBuf::from("unused"), manager: VmManager::new(), capture_number: 0,
            evidence_prefix: "guest-test".into() };
        assert!(!session.owns_vm());
        assert!(session.owned_adb().is_err());
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(session.cleanup()).unwrap();
        assert!(session.manager.running_ids().is_empty());
    }

    #[test]
    fn eof_and_bad_input_close_unowned_session_without_spawning() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for (input, expected) in [(b"".as_slice(), 0), (b"{bad json}\n".as_slice(), 1),
            (b"{\"op\":\"shutdown\"}\n".as_slice(), 0)] {
            let mut session = Session { vm_id: "foreign-fixture".into(), serial: "emulator-5554".into(),
                vm_dir: PathBuf::from("unused"), manager: VmManager::new(), capture_number: 0,
                evidence_prefix: "guest-test".into() };
            let mut output = Vec::new();
            assert_eq!(runtime.block_on(session.run(&mut Cursor::new(input), &mut output)), expected);
            let records: Vec<Value> = output.split(|byte| *byte == b'\n').filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice(line).unwrap()).collect();
            assert_eq!(records.first().unwrap()["event"], "ready");
            assert_eq!(records.last().unwrap()["event"], "closed");
            assert_eq!(records.last().unwrap()["owned"], false);
        }
    }

    #[test]
    fn output_failure_closes_and_unowned_stop_is_a_noop() {
        struct ClosedPipe;
        impl Write for ClosedPipe {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "fixture closed"))
            }
            fn flush(&mut self) -> io::Result<()> { Ok(()) }
        }
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut session = Session { vm_id: "foreign-fixture".into(), serial: "emulator-5554".into(),
            vm_dir: PathBuf::from("unused"), manager: VmManager::new(), capture_number: 0,
            evidence_prefix: "guest-test".into() };
        let stopped = runtime.block_on(session.execute(Operation::Stop)).unwrap();
        assert_eq!(stopped["reason"], "not_owned");
        assert_eq!(runtime.block_on(session.run(&mut Cursor::new(b""), &mut ClosedPipe)), 1);
        assert!(!session.owns_vm());
    }

    #[test]
    fn startup_evidence_validation_is_read_only_and_requires_directories() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("roost-headless-startup-{}-{stamp}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        assert_eq!(validate_vm_evidence_dir(&root).unwrap(), root);
        assert!(!root.join("evidence").exists());
        fs::create_dir(root.join("evidence")).unwrap();
        assert_eq!(validate_vm_evidence_dir(&root).unwrap(), root);
        let invalid_vm = root.join("file-fixture");
        fs::write(&invalid_vm, b"not a VM directory").unwrap();
        assert!(validate_vm_evidence_dir(&invalid_vm).is_err());
        let invalid_evidence = root.join("invalid-evidence-fixture");
        fs::create_dir(&invalid_evidence).unwrap();
        fs::write(invalid_evidence.join("evidence"), b"not an evidence directory").unwrap();
        assert!(validate_vm_evidence_dir(&invalid_evidence).is_err());
        // Isolated fixtures remain available; no recursive deletion is used.
    }

    #[test]
    fn evidence_rejects_paths_and_preexisting_names() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("roost-headless-evidence-{}-{stamp}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let dir = fs::canonicalize(dir).unwrap();
        for name in ["../capture.png", "guest-../x.png", "guest-C:foo.png", "guest-x.txt"] {
            assert!(create_evidence_file(&dir, name).is_err());
        }
        let (path, file) = create_evidence_file(&dir, "guest-test.png").unwrap();
        drop(file);
        assert_eq!(path.parent(), Some(dir.join("evidence").as_path()));
        assert!(create_evidence_file(&dir, "guest-test.png").is_err());
        // Keep isolated fixture evidence for inspection; no recursive deletion.
    }
}