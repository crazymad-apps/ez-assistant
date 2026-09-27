use std::time::Duration;

use mac_usernotifications::{
    AuthorizationStatus, Notification, check_bundle, close_delivered, get_notification_settings,
    request_auth,
};
use tauri::{AppHandle, ipc::Channel};
use tokio_util::sync::CancellationToken;

use super::{NotificationError, NotificationEvent, Permission};

pub(super) async fn permission(
    _: &AppHandle,
    request: bool,
) -> Result<Permission, NotificationError> {
    // 非 .app 二进制调用 Apple API 会失败；先检查，绝不借用 Terminal/Finder 的通知身份。
    if check_bundle().is_err() {
        return Ok(Permission::Unavailable);
    }
    let mut settings = get_notification_settings()
        .await
        .map_err(|_| NotificationError::Unavailable)?;
    if request && settings.authorization_status == AuthorizationStatus::NotDetermined {
        request_auth()
            .await
            .map_err(|_| NotificationError::Unavailable)?;
        settings = get_notification_settings()
            .await
            .map_err(|_| NotificationError::Unavailable)?;
    }
    Ok(match settings.authorization_status {
        AuthorizationStatus::NotDetermined => Permission::Default,
        AuthorizationStatus::Denied => Permission::Denied,
        AuthorizationStatus::Authorized
        | AuthorizationStatus::Provisional
        | AuthorizationStatus::Ephemeral => Permission::Granted,
        AuthorizationStatus::Unknown => Permission::Unavailable,
    })
}

pub(super) async fn show(
    app: &AppHandle,
    id: &str,
    title: &str,
    body: &str,
    events: &Channel<NotificationEvent>,
    cancel: CancellationToken,
) -> Result<(), NotificationError> {
    if permission(app, false).await? != Permission::Granted {
        return Err(NotificationError::PermissionDenied);
    }
    if cancel.is_cancelled() {
        return Ok(());
    }
    let handle = Notification::new()
        .id(id)
        .title(title)
        .message(body)
        .default_sound()
        .send()
        .await
        .map_err(|_| NotificationError::Unavailable)?;
    let result = async {
        events.send(NotificationEvent::Submitted).map_err(|_| NotificationError::Unavailable)?;
        tokio::select! {
            _ = cancel.cancelled() => {},
            _ = tokio::time::sleep(Duration::from_secs(86400)) => {},
            response = handle.response() => {
                let response = response.map_err(|_| NotificationError::Unavailable)?;
                if response.is_default_action() && !cancel.is_cancelled() {
                    // 复用现有窗口唤起；页面再校验用户、Host 和业务事项，OS 回调不执行审批。
                    crate::desktop_lifecycle::show_desktop_window(app.clone()).map_err(|_| NotificationError::Unavailable)?;
                    events.send(NotificationEvent::Clicked).map_err(|_| NotificationError::Unavailable)?;
                }
            },
        }
        Ok(())
    }.await;
    close_delivered(id).await;
    result
}
