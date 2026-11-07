
//! Background utilities must never create a console or take desktop focus.
//! CREATE_NO_WINDOW hides only the console; it does not hide GUI VM windows.

pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

pub fn async_command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    command(program).into()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::Duration;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetConsoleWindow() -> *mut std::ffi::c_void;
    }

    #[test]
    #[ignore = "Subprocess fixture, exercised by background command tests"]
    fn no_console_worker() {
        assert_eq!(std::env::var("ROOST_NO_CONSOLE_FIXTURE").as_deref(), Ok("1"));
        assert!(unsafe { GetConsoleWindow() }.is_null(), "Background utility inherited or created a console");
        println!("ROOST_NO_CONSOLE_CONFIRMED");
    }

    fn fixture_args(command: &mut std::process::Command) {
        command.args(["--exact", "background::tests::no_console_worker", "--ignored", "--nocapture"])
            .env("ROOST_NO_CONSOLE_FIXTURE", "1").stdin(Stdio::null());
    }

    #[test]
    fn sync_background_child_has_no_console() {
        let mut child = command(std::env::current_exe().unwrap());
        fixture_args(&mut child);
        let output = child.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("ROOST_NO_CONSOLE_CONFIRMED"));
    }

    #[tokio::test]
    async fn async_background_child_has_no_console() {
        let mut child = async_command(std::env::current_exe().unwrap());
        child.args(["--exact", "background::tests::no_console_worker", "--ignored", "--nocapture"])
            .env("ROOST_NO_CONSOLE_FIXTURE", "1").stdin(Stdio::null()).kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(15), child.output()).await.unwrap().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("ROOST_NO_CONSOLE_CONFIRMED"));
    }
}