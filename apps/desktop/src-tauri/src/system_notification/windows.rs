#[path = "windows_settings.rs"]
mod settings;

use std::time::Duration;

use tauri::{AppHandle, ipc::Channel};
use tokio_util::sync::CancellationToken;
use windows::{
    Data::Xml::Dom::XmlDocument,
    Foundation::TypedEventHandler,
    UI::Notifications::{
        NotificationSetting, ToastDismissalReason, ToastNotification, ToastNotificationManager,
    },
    core::HSTRING,
};

use super::{NotificationError, NotificationEvent, Permission};

#[allow(unsafe_code)]
pub(super) fn is_focused() -> Result<bool, NotificationError> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };
    // SAFETY: 只读系统当前前台 HWND 和所属 PID，不解引用、保存或关闭借用句柄。
    // 输出指针指向栈上 u32；两个同步调用无需 UI 线程，不产生线程或修改系统状态。
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.is_null() {
        return Ok(false);
    }
    let mut process_id = 0;
    if unsafe { GetWindowThreadProcessId(foreground, &mut process_id) } == 0 {
        return Err(NotificationError::Unavailable);
    }
    // 判断整个客户端是否前台，包含 WebView、资源网页和原生对话框，不要求输入框有焦点。
    Ok(process_id == std::process::id())
}

pub(super) async fn permission(app: &AppHandle, _: bool) -> Result<Permission, NotificationError> {
    let setting = settings::read(&app.config().identifier).map_err(|error| {
        eprintln!("Windows notification settings failed: {:?}", error.code());
        NotificationError::Unavailable
    })?;
    Ok(if setting == NotificationSetting::Enabled {
        Permission::Granted
    } else {
        Permission::Denied
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
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
        &app.config().identifier,
    ))
    .map_err(|_| NotificationError::Unavailable)?;
    let xml = XmlDocument::new().map_err(|_| NotificationError::Unavailable)?;
    xml.LoadXml(&HSTRING::from(format!("<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>", escape(title), escape(body))))
        .map_err(|_| NotificationError::Unavailable)?;
    let toast = ToastNotification::CreateToastNotification(&xml)
        .map_err(|_| NotificationError::Unavailable)?;
    // OS tag 有长度限制；JS 的完整不透明 id 只用于进程内取消。
    toast
        .SetTag(&HSTRING::from(&id[..id.len().min(16)]))
        .map_err(|_| NotificationError::Unavailable)?;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let activated_sender = sender.clone();
    let failed_sender = sender.clone();
    let activated = toast
        .Activated(&TypedEventHandler::new(move |_, _| {
            let _ = activated_sender.try_send(Ok(true));
            Ok(())
        }))
        .map_err(|_| NotificationError::Unavailable)?;
    let dismissed =
        toast
            .Dismissed(&TypedEventHandler::new(
                move |_,
                      args: windows::core::Ref<
                    windows::UI::Notifications::ToastDismissedEventArgs,
                >| {
                    if let Some(args) = args.as_ref()
                        && args.Reason()? != ToastDismissalReason::TimedOut
                    {
                        let _ = sender.try_send(Ok(false));
                    }
                    Ok(())
                },
            ))
            .map_err(|_| NotificationError::Unavailable)?;
    let failed = toast
        .Failed(&TypedEventHandler::new(move |_, _| {
            let _ = failed_sender.try_send(Err(NotificationError::Unavailable));
            Ok(())
        }))
        .map_err(|_| NotificationError::Unavailable)?;
    let mut shown = false;
    let result = async {
        // JS 的预检查不能覆盖切回窗口的竞态；紧邻 OS Show 再读当前前台进程。
        if is_focused()? {
            events.send(NotificationEvent::Skipped).map_err(|_| NotificationError::Unavailable)?;
            return Ok(());
        }
        notifier.Show(&toast).map_err(|_| NotificationError::Unavailable)?;
        shown = true;
        events.send(NotificationEvent::Submitted).map_err(|_| NotificationError::Unavailable)?;
        tokio::select! {
            _ = cancel.cancelled() => {},
            _ = tokio::time::sleep(Duration::from_secs(86400)) => {},
            response = receiver.recv() => {
                if response.ok_or(NotificationError::Unavailable)?? && !cancel.is_cancelled() {
                    crate::desktop_lifecycle::show_desktop_window(app.clone()).map_err(|_| NotificationError::Unavailable)?;
                    events.send(NotificationEvent::Clicked).map_err(|_| NotificationError::Unavailable)?;
                }
            },
        }
        Ok(())
    }.await;
    let _ = toast.RemoveActivated(activated);
    let _ = toast.RemoveDismissed(dismissed);
    let _ = toast.RemoveFailed(failed);
    if shown {
        notifier
            .Hide(&toast)
            .map_err(|_| NotificationError::Unavailable)?;
    }
    result
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
