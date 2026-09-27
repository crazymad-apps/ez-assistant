use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::{
    Data::Xml::Dom::XmlDocument,
    Foundation::{DateTime, IReference, PropertyValue},
    UI::Notifications::{NotificationSetting, ToastNotification, ToastNotificationManager},
    core::{HSTRING, Interface},
};

// Win32 应用首次发送前，Setting 可能返回 ERROR_NOT_FOUND，不能将其当作权限拒绝。
const SETTINGS_NOT_INITIALIZED: windows::core::HRESULT =
    windows::core::HRESULT(0x80070490_u32 as i32);

pub(super) fn read(app_id: &str) -> windows::core::Result<NotificationSetting> {
    let app_id = HSTRING::from(app_id);
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&app_id)?;
    match notifier.Setting() {
        Err(error) if error.code() == SETTINGS_NOT_INITIALIZED => {
            // 采用 Windows Community Toolkit 的首次注册方式：无横幅、无声音，立即移除，
            // 并设置短期过期时间防止清理失败残留。只初始化本应用，不改系统通知偏好；
            // 最终仍由 Setting 判断禁用/策略状态，其他错误原样返回。
            // https://github.com/CommunityToolkit/WindowsCommunityToolkit/blob/main/Microsoft.Toolkit.Uwp.Notifications/Toasts/Compat/ToastNotificationManagerCompat.cs
            let xml = XmlDocument::new()?;
            xml.LoadXml(&HSTRING::from(
                "<toast><visual><binding template=\"ToastGeneric\"><text>EZ Assistant</text></binding></visual><audio silent=\"true\"/></toast>",
            ))?;
            let toast = ToastNotification::CreateToastNotification(&xml)?;
            let tag = HSTRING::from("initialization");
            let group = HSTRING::from("ez-registration");
            toast.SetTag(&tag)?;
            toast.SetGroup(&group)?;
            toast.SetSuppressPopup(true)?;
            let expires = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
                windows::core::Error::from_hresult(windows::core::HRESULT(0x80070057_u32 as i32))
            })? + Duration::from_secs(15);
            // WinRT DateTime 是从 1601-01-01 起算的 100ns 数；异常系统时间返回错误。
            let ticks = (expires.as_nanos() / 100)
                .checked_add(116_444_736_000_000_000)
                .and_then(|value| i64::try_from(value).ok())
                .ok_or_else(|| {
                    windows::core::Error::from_hresult(windows::core::HRESULT(
                        0x80070057_u32 as i32,
                    ))
                })?;
            let expiration: IReference<DateTime> = PropertyValue::CreateDateTime(DateTime {
                UniversalTime: ticks,
            })?
            .cast()?;
            toast.SetExpirationTime(&expiration)?;
            let history = ToastNotificationManager::History()?;
            notifier.Show(&toast)?;
            history.RemoveGroupedTagWithId(&tag, &group, &app_id)?;
            notifier.Setting()
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "使用独立测试身份调用真实 Windows 通知 API，需在 Windows 验收机显式运行"]
    fn first_permission_read_initializes_identity_and_removes_temporary_toast() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let app_id = format!(
            "com.ez-assistant.desktop.test.{}.{unique}",
            std::process::id()
        );
        let notifier =
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(&app_id)).unwrap();
        assert_eq!(
            notifier.Setting().unwrap_err().code(),
            SETTINGS_NOT_INITIALIZED
        );

        // 不假定验收机必然允许通知；返回值须与系统设置一致，重复读取不得重新发通知。
        let setting = read(&app_id).unwrap();
        assert_eq!(notifier.Setting().unwrap(), setting);
        assert_eq!(read(&app_id).unwrap(), setting);
        let history = ToastNotificationManager::History()
            .unwrap()
            .GetHistoryWithId(&HSTRING::from(&app_id))
            .unwrap();
        assert_eq!(history.Size().unwrap(), 0);
    }
}
