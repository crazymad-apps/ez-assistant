//! 受控外部 HTTP(S) 链接与固定产品目录打开边界。

use assistant_protocol::WorkspaceId;
use serde::Serialize;
use std::path::PathBuf;
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

#[tauri::command]
pub(crate) async fn skill_directory_path(
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    workspace_id: Option<String>,
    source: String,
) -> Result<String, ExternalLinkFailure> {
    resolve_skill_directory(&coordinator, workspace_id, &source)
        .await
        .map(|path| path.to_string_lossy().into_owned())
}

#[tauri::command]
pub(crate) async fn open_skill_directory(
    app: AppHandle,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    workspace_id: Option<String>,
    source: String,
) -> Result<(), ExternalLinkFailure> {
    let path = resolve_skill_directory(&coordinator, workspace_id, &source).await?;
    if !path.is_dir() {
        return Err(ExternalLinkFailure {
            code: "skill_directory_missing",
            message: "技能来源目录不存在。",
        });
    }
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|_| ExternalLinkFailure {
            code: "skill_directory_open_failed",
            message: "无法使用系统文件管理器打开技能来源目录。",
        })
}

async fn resolve_skill_directory(
    coordinator: &RuntimeBootstrapCoordinator,
    workspace_id: Option<String>,
    source: &str,
) -> Result<PathBuf, ExternalLinkFailure> {
    let (root, relative) = match source {
        "workspace_ez_assistant" => (
            workspace_root(coordinator, workspace_id).await?,
            ".ez-assistant/skills",
        ),
        "workspace_agents" => (
            workspace_root(coordinator, workspace_id).await?,
            ".agents/skills",
        ),
        "user_ez_assistant" => (user_home()?, ".ez-assistant/skills"),
        "user_agents" => (user_home()?, ".agents/skills"),
        _ => {
            return Err(ExternalLinkFailure {
                code: "invalid_skill_source",
                message: "技能来源类型无效。",
            });
        }
    };
    Ok(root.join(relative))
}

async fn workspace_root(
    coordinator: &RuntimeBootstrapCoordinator,
    workspace_id: Option<String>,
) -> Result<PathBuf, ExternalLinkFailure> {
    let workspace_id = workspace_id.ok_or(ExternalLinkFailure {
        code: "workspace_required",
        message: "请先选择工作区。",
    })?;
    let workspace_id = WorkspaceId::new(workspace_id).map_err(|_| ExternalLinkFailure {
        code: "invalid_workspace",
        message: "工作区标识无效。",
    })?;
    coordinator
        .workspace_directory(workspace_id)
        .await
        .map(PathBuf::from)
        .map_err(|_| ExternalLinkFailure {
            code: "workspace_unavailable",
            message: "无法从运行时获取工作区目录。",
        })
}

fn user_home() -> Result<PathBuf, ExternalLinkFailure> {
    dirs::home_dir().ok_or(ExternalLinkFailure {
        code: "user_home_unavailable",
        message: "无法确定用户目录。",
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

    #[tokio::test]
    async fn skill_directory_source_cannot_escape_the_fixed_allowlist() {
        let coordinator = RuntimeBootstrapCoordinator::for_application();
        let error = resolve_skill_directory(&coordinator, None, "../../private")
            .await
            .expect_err("arbitrary source must be rejected");
        assert_eq!(error.code, "invalid_skill_source");
    }
}
