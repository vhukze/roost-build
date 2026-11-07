// 防止 Windows 下弹出控制台窗口
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    if let Some(exit_code) = droidbox_lib::headless::dispatch(std::env::args_os().skip(1)) {
        std::process::exit(exit_code);
    }
    droidbox_lib::run()
}
