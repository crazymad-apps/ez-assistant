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
