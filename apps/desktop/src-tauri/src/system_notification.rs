//! 客户端系统通知适配。只持有 OS 句柄与取消信号，不接管 Runtime 待办状态。

#[cfg(target_os = "macos")]
#[path = "system_notification/macos.rs"]
mod platform;
#[cfg(windows)]
#[path = "system_notification/windows.rs"]
mod platform;

use std::{collections::HashMap, sync::Mutex};

use serde::Serialize;
use tauri::{AppHandle, State, ipc::Channel};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(crate) struct SystemNotifications {
    active: Mutex<HashMap<String, CancellationToken>>,
}

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Permission {
    #[cfg(target_os = "macos")]
    Default,
    #[cfg(any(target_os = "macos", windows))]
    Granted,
    #[cfg(any(target_os = "macos", windows))]
    Denied,
    #[cfg(not(any(target_os = "macos", windows)))]
    Unsupported,
    #[cfg(target_os = "macos")]
    Unavailable,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum NotificationEvent {
    #[cfg(any(target_os = "macos", windows))]
    Submitted,
    #[cfg(any(target_os = "macos", windows))]
    Clicked,
}

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NotificationError {
    #[error("系统通知当前不可用，请检查应用安装状态与权限")]
    Unavailable,
    #[cfg(any(target_os = "macos", windows))]
    #[error("通知权限未开启")]
    PermissionDenied,
    #[error("通知参数无效或数量已达上限")]
    InvalidRequest,
}

#[tauri::command]
pub(crate) async fn system_notification_permission(
    app: AppHandle,
    request: bool,
) -> Result<Permission, NotificationError> {
    #[cfg(any(target_os = "macos", windows))]
    return platform::permission(&app, request).await;
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (app, request);
        Ok(Permission::Unsupported)
    }
}

#[tauri::command]
pub(crate) async fn show_system_notification(
    app: AppHandle,
    state: State<'_, SystemNotifications>,
    id: String,
    title: String,
    body: String,
    events: Channel<NotificationEvent>,
) -> Result<(), NotificationError> {
    if !valid_request(&id, &title, &body) {
        return Err(NotificationError::InvalidRequest);
    }
    let cancel = CancellationToken::new();
    {
        let mut active = state
            .active
            .lock()
            .map_err(|_| NotificationError::Unavailable)?;
        if active.len() >= 64 || active.contains_key(&id) {
            return Err(NotificationError::InvalidRequest);
        }
        active.insert(id.clone(), cancel.clone());
    }
    // Tauri command future 拥有等待生命周期；每项可取消且最长一天，不创建 detached 任务或阻塞线程。
    #[cfg(any(target_os = "macos", windows))]
    let result = platform::show(&app, &id, &title, &body, &events, cancel).await;
    #[cfg(not(any(target_os = "macos", windows)))]
    let result = {
        let _ = (app, title, body, events, cancel);
        Err(NotificationError::Unavailable)
    };
    state
        .active
        .lock()
        .map_err(|_| NotificationError::Unavailable)?
        .remove(&id);
    result
}

#[tauri::command]
pub(crate) fn close_system_notification(
    state: State<'_, SystemNotifications>,
    id: String,
) -> Result<(), NotificationError> {
    if let Some(cancel) = state
        .active
        .lock()
        .map_err(|_| NotificationError::Unavailable)?
        .get(&id)
    {
        cancel.cancel();
    }
    Ok(())
}

impl SystemNotifications {
    pub(crate) fn cancel_all(&self) {
        if let Ok(active) = self.active.lock() {
            for cancel in active.values() {
                cancel.cancel();
            }
        }
    }
}

fn valid_request(id: &str, title: &str, body: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !title.is_empty()
        && title.len() <= 200
        && body.len() <= 500
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_bounded_opaque_identifiers_and_summary() {
        assert!(valid_request("123-ab", "需要你的审批", "点击返回会话"));
        assert!(!valid_request("../../file", "title", "body"));
        assert!(!valid_request("id", "", "body"));
        assert!(!valid_request("id", "title", &"x".repeat(501)));
    }
}
