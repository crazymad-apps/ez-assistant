//! EZ Assistant 桌面进程入口。
//!
//! 本 crate 只负责 Tauri/WebView 与桌面平台适配；Session、Run 和 Agent 权威状态属于
//! 独立 Assistant Runtime Host。

mod desktop_lifecycle;
mod desktop_preferences;
mod external_link;
mod native_resource;
mod runtime_bootstrap;
mod workspace_directory;

use tauri::Manager as _;

#[tauri::command]
fn health() -> &'static str {
    "Rust runtime connected"
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let runtime_bootstrap = runtime_bootstrap::RuntimeBootstrapCoordinator::for_application();
    let native_resources = native_resource::NativeResourceBridge::new();
    let desktop_lifecycle = desktop_lifecycle::DesktopLifecycleCoordinator::new();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .manage(runtime_bootstrap)
        .manage(native_resources)
        .manage(desktop_lifecycle)
        .setup(|app| {
            let app_handle = app.handle().clone();
            let coordinator = app.state::<desktop_lifecycle::DesktopLifecycleCoordinator>();
            coordinator.install(&app_handle)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            let coordinator = window.state::<desktop_lifecycle::DesktopLifecycleCoordinator>();
            coordinator.handle_window_event(window.app_handle(), window, event);
        })
        .invoke_handler(tauri::generate_handler![
            health,
            desktop_lifecycle::desktop_platform,
            desktop_lifecycle::is_desktop_window_maximized,
            desktop_lifecycle::minimize_desktop_window,
            desktop_lifecycle::quit_desktop,
            desktop_lifecycle::request_desktop_close,
            desktop_lifecycle::show_desktop_window,
            desktop_lifecycle::take_pending_desktop_lifecycle_intent,
            desktop_lifecycle::toggle_maximize_desktop_window,
            desktop_lifecycle::update_native_runtime_state,
            desktop_preferences::load_desktop_preferences,
            desktop_preferences::save_desktop_preferences,
            external_link::open_external_http_url,
            external_link::open_session_workspace_directory,
            external_link::open_skill_directory,
            external_link::open_workspace_directory,
            external_link::skill_directory_path,
            native_resource::cancel_resource_operation,
            native_resource::choose_attachment_files,
            native_resource::export_session_markdown,
            native_resource::materialize_new_session,
            native_resource::open_attachment_in_system,
            native_resource::open_tool_file_in_system,
            native_resource::preview_attachment,
            native_resource::preview_attachment_selection,
            native_resource::thumbnail_attachment,
            native_resource::preview_tool_file,
            native_resource::reveal_attachment_in_directory,
            native_resource::reveal_tool_file_in_directory,
            native_resource::release_attachment_selection,
            native_resource::stage_clipboard_image,
            native_resource::upload_selected_attachment,
            runtime_bootstrap::bootstrap_runtime,
            runtime_bootstrap::restart_runtime,
            runtime_bootstrap::stop_runtime,
            runtime_bootstrap::open_runtime_home,
            workspace_directory::choose_workspace_directory
        ])
        .build(tauri::generate_context!())
        .expect("failed to build EZ Assistant");
    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let coordinator = app.state::<desktop_lifecycle::DesktopLifecycleCoordinator>();
            if coordinator.should_prevent_exit() {
                api.prevent_exit();
                desktop_lifecycle::request_intent(
                    app,
                    desktop_lifecycle::DesktopLifecycleIntent::QuitDesktop,
                );
            }
        }
    });
}
