//! EZ Assistant 桌面进程入口。
//!
//! 本 crate 只负责 Tauri/WebView 与桌面平台适配；Session、Run 和 Agent 权威状态属于
//! 独立 Assistant Runtime Host。

mod desktop_preferences;
mod external_link;
mod native_resource;
mod runtime_bootstrap;
mod workspace_directory;

#[tauri::command]
fn health() -> &'static str {
    "Rust runtime connected"
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let runtime_bootstrap = runtime_bootstrap::RuntimeBootstrapCoordinator::for_application();
    let native_resources = native_resource::NativeResourceBridge::new();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(runtime_bootstrap)
        .manage(native_resources)
        .invoke_handler(tauri::generate_handler![
            health,
            desktop_preferences::load_desktop_preferences,
            desktop_preferences::save_desktop_preferences,
            external_link::open_external_http_url,
            external_link::open_workspace_directory,
            native_resource::cancel_resource_operation,
            native_resource::choose_attachment_files,
            native_resource::export_session_markdown,
            native_resource::open_attachment_in_system,
            native_resource::open_tool_file_in_system,
            native_resource::preview_attachment,
            native_resource::preview_tool_file,
            native_resource::reveal_attachment_in_directory,
            native_resource::reveal_tool_file_in_directory,
            native_resource::release_attachment_selection,
            native_resource::upload_selected_attachment,
            runtime_bootstrap::bootstrap_runtime,
            workspace_directory::choose_workspace_directory
        ])
        .run(tauri::generate_context!())
        .expect("failed to run EZ Assistant");
}
