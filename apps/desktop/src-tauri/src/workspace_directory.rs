use serde::Serialize;
use tauri_plugin_dialog::DialogExt;

#[derive(Debug, Serialize)]
pub(crate) struct WorkspaceDirectoryFailure {
    code: &'static str,
    message: &'static str,
}

impl WorkspaceDirectoryFailure {
    fn unavailable() -> Self {
        Self {
            code: "workspace_directory_unavailable",
            message: "无法读取所选工作空间目录。",
        }
    }
}

#[tauri::command]
pub(crate) async fn choose_workspace_directory(
    app: tauri::AppHandle,
) -> Result<Option<String>, WorkspaceDirectoryFailure> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("选择工作空间目录")
        .pick_folder(move |selection| {
            let _ = sender.send(selection);
        });

    let Some(selection) = receiver
        .await
        .map_err(|_| WorkspaceDirectoryFailure::unavailable())?
    else {
        return Ok(None);
    };
    let path = selection
        .into_path()
        .map_err(|_| WorkspaceDirectoryFailure::unavailable())?;
    let directory = path
        .to_str()
        .ok_or_else(WorkspaceDirectoryFailure::unavailable)?;
    Ok(Some(directory.to_owned()))
}
