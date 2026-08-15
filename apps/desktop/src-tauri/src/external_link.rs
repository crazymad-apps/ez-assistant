//! 受控外部 HTTP(S) 链接打开边界。

use assistant_protocol::WorkspaceId;
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use url::Url;

use crate::runtime_bootstrap::RuntimeBootstrapCoordinator;

#[derive(Debug, Serialize)]
pub(crate) struct ExternalLinkFailure {
    code: &'static str,
    message: &'static str,
}

#[tauri::command]
pub(crate) fn open_external_http_url(
    app: AppHandle,
    url: String,
) -> Result<(), ExternalLinkFailure> {
    let parsed = Url::parse(&url).map_err(|_| invalid_url())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(invalid_url());
    }
    app.opener()
        .open_url(parsed.as_str(), None::<&str>)
        .map_err(|_| ExternalLinkFailure {
            code: "external_open_failed",
            message: "无法使用系统浏览器打开该链接。",
        })
}

#[tauri::command]
pub(crate) async fn open_workspace_directory(
    app: AppHandle,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    workspace_id: String,
) -> Result<(), ExternalLinkFailure> {
    let workspace_id = WorkspaceId::new(workspace_id).map_err(|_| ExternalLinkFailure {
        code: "invalid_workspace",
        message: "工作空间标识无效。",
    })?;
    let path = coordinator
        .workspace_directory(workspace_id)
        .await
        .map_err(|_| ExternalLinkFailure {
            code: "workspace_unavailable",
            message: "无法从 Runtime 获取工作空间目录。",
        })?;
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|_| ExternalLinkFailure {
            code: "workspace_open_failed",
            message: "无法使用系统文件管理器打开工作空间。",
        })
}

fn invalid_url() -> ExternalLinkFailure {
    ExternalLinkFailure {
        code: "invalid_external_url",
        message: "只允许打开有效的 HTTP 或 HTTPS 链接。",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_schemes_before_opening() {
        for value in [
            "file:///tmp/a",
            "javascript:alert(1)",
            "data:text/plain,test",
        ] {
            let parsed = Url::parse(value).expect("fixture URL");
            assert!(!matches!(parsed.scheme(), "http" | "https"));
        }
    }
}
