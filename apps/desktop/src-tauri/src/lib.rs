//! EZ Assistant 桌面进程入口。
//!
//! 本 crate 只负责 Tauri/WebView 与桌面平台适配；Session、Run 和 Agent 权威状态属于
//! 独立 Assistant Runtime Host。

#[tauri::command]
fn health() -> &'static str {
    "Rust runtime connected"
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![health])
        .run(tauri::generate_context!())
        .expect("failed to run EZ Assistant");
}
