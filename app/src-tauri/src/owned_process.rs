
//! Windows-only process-tree ownership for processes launched by this manager.
//! A fresh unnamed Job is attached before the initial thread is resumed.
//! No process-name matching, PID-only termination, breakaway, or kill-on-close.
use std::ffi::c_void;
use std::io;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use tokio::process::Child;

type Handle = *mut c_void;

#[repr(C)]
struct ThreadEntry {
    size: u32,
    usage: u32,
    thread_id: u32,
    owner_pid: u32,
    base_priority: i32,
    delta_priority: i32,
    flags: u32,
}

#[repr(C)]
#[derive(Default)]
struct JobAccounting {
    user_time: i64,
    kernel_time: i64,
    period_user_time: i64,
    period_kernel_time: i64,
    page_faults: u32,
    total_processes: u32,
    active_processes: u32,
    terminated_processes: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
    fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    fn QueryInformationJobObject(job: Handle, class: i32, information: *mut c_void, length: u32, returned: *mut u32) -> i32;
    fn TerminateJobObject(job: Handle, exit_code: u32) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
    fn Thread32First(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
    fn Thread32Next(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
    fn OpenThread(access: u32, inherit: i32, thread_id: u32) -> Handle;
    fn ResumeThread(thread: Handle) -> u32;
}

/// The integer is a non-inheritable owned kernel handle, not a PID. Moving this
/// value across tasks does not affect object identity or require thread affinity.
struct OwnedHandle(isize);
impl OwnedHandle {
    fn new(handle: Handle) -> io::Result<Self> {
        if handle.is_null() || handle as isize == -1 { Err(io::Error::last_os_error()) }
        else { Ok(Self(handle as isize)) }
    }
    fn raw(&self) -> Handle { self.0 as Handle }
}
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: this wrapper uniquely owns a valid handle returned by Win32.
        unsafe { CloseHandle(self.raw()); }
    }
}

pub struct ProcessJob(OwnedHandle);
impl ProcessJob {
    pub fn new() -> io::Result<Self> {
        // SAFETY: null attributes create a non-inheritable unnamed Job.
        OwnedHandle::new(unsafe { CreateJobObjectW(null(), null()) }).map(Self)
    }

    pub fn assign(&self, child: &Child) -> io::Result<()> {
        let process = child.raw_handle().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "new suspended child handle missing"))?;
        // SAFETY: child retains the process handle and this object retains Job.
        if unsafe { AssignProcessToJobObject(self.0.raw(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn active_processes(&self) -> io::Result<u32> {
        let mut accounting = JobAccounting::default();
        // SAFETY: repr(C) matches JOBOBJECT_BASIC_ACCOUNTING_INFORMATION;
        // JobObjectBasicAccountingInformation is class 1.
        let ok = unsafe { QueryInformationJobObject(self.0.raw(), 1,
            &mut accounting as *mut JobAccounting as *mut c_void,
            size_of::<JobAccounting>() as u32, null_mut()) };
        if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(accounting.active_processes) }
    }

    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: this unnamed Job contains only the suspended launcher and its
        // inherited descendants; no unrelated existing process is assigned.
        if unsafe { TerminateJobObject(self.0.raw(), 1) } == 0 { return Err(io::Error::last_os_error()); }
        Ok(())
    }
}

/// Tokio exposes the process handle but not the initial thread handle. Enumerate
/// only to obtain that thread; the child process handle remains held throughout,
/// preventing PID reuse. Reject ambiguity instead of resuming unrelated threads.
pub fn resume_initial_thread(child: &Child) -> io::Result<()> {
    let pid = child.id().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "suspended child PID missing"))?;
    // SAFETY: documented TH32CS_SNAPTHREAD snapshot, no target mutation.
    let snapshot = OwnedHandle::new(unsafe { CreateToolhelp32Snapshot(0x0000_0004, 0) })?;
    // SAFETY: all fields are scalar integers and zero is a valid initial value.
    let mut entry: ThreadEntry = unsafe { zeroed() };
    entry.size = size_of::<ThreadEntry>() as u32;
    let mut thread_ids = Vec::new();
    // SAFETY: initialized repr(C) structure and valid snapshot retained by RAII.
    let mut found = unsafe { Thread32First(snapshot.raw(), &mut entry) } != 0;
    while found {
        if entry.owner_pid == pid { thread_ids.push(entry.thread_id); }
        entry.size = size_of::<ThreadEntry>() as u32;
        found = unsafe { Thread32Next(snapshot.raw(), &mut entry) } != 0;
    }
    if thread_ids.len() != 1 {
        return Err(io::Error::new(io::ErrorKind::Other, format!("expected one suspended initial thread, found {}", thread_ids.len())));
    }
    // THREAD_SUSPEND_RESUME only; never open any thread outside our held child.
    let thread = OwnedHandle::new(unsafe { OpenThread(0x0002, 0, thread_ids[0]) })?;
    let previous = unsafe { ResumeThread(thread.raw()) };
    if previous == u32::MAX { return Err(io::Error::last_os_error()); }
    if previous != 1 {
        return Err(io::Error::new(io::ErrorKind::Other, format!("unexpected initial suspend count: {previous}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    use tokio::process::Command;

    const WORKER: &str = "owned_process::tests::job_worker";

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn IsProcessInJob(process: Handle, job: Handle, result: *mut i32) -> i32;
        fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    }

    fn retained_fixture_process(dir: &Path, role: &str, job: &ProcessJob) -> OwnedHandle {
        let pid: u32 = fs::read_to_string(dir.join(format!("{role}.ready"))).unwrap().trim().parse().unwrap();
        // Retain only QUERY_LIMITED_INFORMATION | SYNCHRONIZE for the fixture.
        // All termination remains scoped to its test-created Job, never this PID.
        let process = OwnedHandle::new(unsafe { OpenProcess(0x0010_1000, 0, pid) }).unwrap();
        let mut member = 0i32;
        assert_ne!(unsafe { IsProcessInJob(process.raw(), job.0.raw(), &mut member) }, 0);
        assert_eq!(member, 1, "fixture is not a member of the expected Job");
        assert_eq!(unsafe { WaitForSingleObject(process.raw(), 0) }, 258, "fixture already exited");
        process
    }

    #[test]
    fn win32_abi_sizes() {
        assert_eq!(size_of::<ThreadEntry>(), 28);
        assert_eq!(size_of::<JobAccounting>(), 48);
    }

    // Called only as an explicitly selected ignored test in a separate fixture
    // process. No Emulator, ADB, shell, SDK, or user VM paths are involved.
    #[test]
    #[ignore = "subprocess fixture selected by job_tree_survives_launcher_exit"]
    fn job_worker() {
        let role = std::env::var("ROOST_JOB_FIXTURE_ROLE").expect("fixture role");
        let dir = PathBuf::from(std::env::var_os("ROOST_JOB_FIXTURE_DIR").expect("fixture dir"));
        if role == "parent" {
            let stdout = fs::File::create(dir.join("grandchild.stdout")).unwrap();
            let stderr = fs::File::create(dir.join("grandchild.stderr")).unwrap();
            use std::os::windows::process::CommandExt;
            let grandchild = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", WORKER, "--nocapture", "--test-threads=1"])
                .env("ROOST_JOB_FIXTURE_ROLE", "grandchild")
                .env("ROOST_JOB_FIXTURE_DIR", &dir)
                .stdin(Stdio::null()).stdout(Stdio::from(stdout)).stderr(Stdio::from(stderr))
                // The fixture is a console test executable. Avoid creating a
                // conhost descendant that would make the expected Job size 2.
                .creation_flags(0x0800_0000)
                .spawn().expect("spawn fixture grandchild");
            fs::write(dir.join("grandchild.pid"), grandchild.id().to_string()).unwrap();
            // Dropping std::process::Child keeps the descendant alive so the Job
            // must retain ownership even after this launcher exits normally.
            drop(grandchild);
            return;
        }
        assert!(role == "grandchild" || role == "bypass");
        fs::write(dir.join(format!("{role}.ready")), std::process::id().to_string()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(45);
        let mut heartbeat = 0u32;
        while Instant::now() < deadline && !dir.join("stop").exists() {
            heartbeat += 1;
            fs::write(dir.join(format!("{role}.heartbeat")), heartbeat.to_string()).unwrap();
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    struct FixtureCleanup {
        job: ProcessJob,
        dir: PathBuf,
    }
    impl Drop for FixtureCleanup {
        fn drop(&mut self) {
            // On any assertion failure stop only this test-created Job. Keep
            // marker files on the ephemeral CI runner for failure inspection.
            let _ = fs::write(self.dir.join("stop"), b"stop");
            let _ = self.job.terminate();
        }
    }

    fn spawn_fixture(job: &ProcessJob, dir: &Path, role: &str) -> Child {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--ignored", "--exact", WORKER, "--nocapture", "--test-threads=1"])
            .env("ROOST_JOB_FIXTURE_ROLE", role).env("ROOST_JOB_FIXTURE_DIR", dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(fs::File::create(dir.join(format!("{role}.stdout"))).unwrap()))
            .stderr(Stdio::from(fs::File::create(dir.join(format!("{role}.stderr"))).unwrap()))
            .creation_flags(0x0000_0004 | 0x0800_0000).kill_on_drop(true);
        let child = command.spawn().expect("spawn suspended fixture");
        job.assign(&child).expect("assign original Child handle");
        resume_initial_thread(&child).expect("resume initial thread");
        child
    }

    async fn wait_until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if ready() { return; }
            assert!(Instant::now() < deadline, "fixture deadline exceeded");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn job_tree_survives_launcher_exit() {
        // This is actual Rust/Tokio regression coverage, to be compiled and run
        // only by the GitHub Windows workflow. Local syntax parsing is not a pass.
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("roost-job-test-{}-{nonce}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let primary = FixtureCleanup { job: ProcessJob::new().unwrap(), dir: dir.clone() };
        let bypass = FixtureCleanup { job: ProcessJob::new().unwrap(), dir: dir.clone() };
        let mut parent = spawn_fixture(&primary.job, &dir, "parent");
        let mut outside = spawn_fixture(&bypass.job, &dir, "bypass");
        let status = tokio::time::timeout(Duration::from_secs(10), parent.wait()).await.unwrap().unwrap();
        assert!(status.success(), "fixture launcher failed; logs in {dir:?}");
        wait_until(|| dir.join("grandchild.ready").is_file() && dir.join("bypass.ready").is_file()).await;
        // Windows may also assign conhost.exe to a console fixture's Job.
        // Assert exact fixture membership and liveness rather than an OS-dependent
        // process count of 1. The production stop condition remains exactly zero.
        let descendant = retained_fixture_process(&dir, "grandchild", &primary.job);
        let independent = retained_fixture_process(&dir, "bypass", &bypass.job);
        let primary_count = primary.job.active_processes().unwrap();
        let bypass_count = bypass.job.active_processes().unwrap();
        fs::write(dir.join("job-counts.txt"), format!("primary={primary_count}\nbypass={bypass_count}\n")).unwrap();
        assert!(primary_count >= 1, "descendant lost after launcher exit");
        assert!(bypass_count >= 1);
        let descendant_before = fs::read_to_string(dir.join("grandchild.heartbeat")).unwrap_or_default();
        wait_until(|| fs::read_to_string(dir.join("grandchild.heartbeat"))
            .map(|value| !value.is_empty() && value != descendant_before).unwrap_or(false)).await;
        primary.job.terminate().unwrap();
        wait_until(|| primary.job.active_processes().unwrap() == 0).await;
        assert_eq!(unsafe { WaitForSingleObject(descendant.raw(), 0) }, 0, "owned descendant survived termination");
        assert_eq!(unsafe { WaitForSingleObject(independent.raw(), 0) }, 258, "another Job's fixture was affected");
        assert!(outside.try_wait().unwrap().is_none(), "unrelated fixture was terminated");
        let before = fs::read_to_string(dir.join("bypass.heartbeat")).unwrap_or_default();
        wait_until(|| fs::read_to_string(dir.join("bypass.heartbeat")).map(|value| !value.is_empty() && value != before).unwrap_or(false)).await;
        assert!(bypass.job.active_processes().unwrap() >= 1, "another Job was affected");
        fs::write(dir.join("stop"), b"stop").unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(10), outside.wait()).await.unwrap().unwrap().success());
        wait_until(|| bypass.job.active_processes().unwrap() == 0).await;
        eprintln!("Roost Job tree regression passed; fixture logs: {}", dir.display());
    }
}