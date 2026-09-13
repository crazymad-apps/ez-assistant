// 正式 Windows 桌面程序使用 GUI 子系统，避免启动时附带控制台及其关闭事件。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    ez_assistant_desktop_lib::run();
}
