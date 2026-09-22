//! 已验证 Host 身份到浏览器 Profile 的单向选择；旧默认 Profile 只归本机个人域。
use assistant_protocol::{HostIdentityKind, HostLoginResult, HostMode};
use sha2::{Digest, Sha256};
#[cfg(not(target_os = "macos"))]
use tauri::Manager;
use tauri::{AppHandle, webview::WebviewBuilder};

pub(super) fn profile(
    identity: &HostLoginResult,
    local: bool,
    origin: &str,
) -> Result<Option<[u8; 16]>, String> {
    let user = match (&identity.mode, &identity.identity) {
        (HostMode::Personal, None) if local => return Ok(None),
        (HostMode::Personal, None) => "_personal".to_owned(),
        (HostMode::Enterprise, Some(user)) if identity.kind == HostIdentityKind::User => {
            format!("{}_{}", user.center_id, user.user_id)
        }
        _ => return Err("请先登录 Host 后打开网页。".into()),
    };
    let mut hash = Sha256::new();
    hash.update(origin.as_bytes());
    hash.update([0]);
    hash.update(user.as_bytes());
    let digest = hash.finalize();
    Ok(Some(
        digest[..16].try_into().expect("SHA256 contains 16 bytes"),
    ))
}

pub(super) fn configure(
    mut builder: WebviewBuilder<tauri::Wry>,
    app: &AppHandle,
    profile: Option<[u8; 16]>,
) -> Result<WebviewBuilder<tauri::Wry>, String> {
    let Some(profile) = profile else {
        return Ok(builder);
    };
    #[cfg(target_os = "macos")]
    {
        let _ = app;
        // WKWebView 在 macOS 14 前没有命名持久 Store，使用独立非持久 Store，绝不退回默认个人 Store。
        let version = plist::Value::from_file("/System/Library/CoreServices/SystemVersion.plist")
            .ok()
            .and_then(|v| {
                v.as_dictionary()?
                    .get("ProductVersion")?
                    .as_string()?
                    .split('.')
                    .next()?
                    .parse::<u32>()
                    .ok()
            });
        builder = if version.is_some_and(|v| v >= 14) {
            builder.data_store_identifier(profile)
        } else {
            builder.incognito(true)
        };
    }
    #[cfg(not(target_os = "macos"))]
    {
        let name: String = profile.iter().map(|b| format!("{b:02x}")).collect();
        builder = builder.data_directory(
            app.path()
                .app_local_data_dir()
                .map_err(|_| "browser_profile_unavailable")?
                .join("browser-profiles")
                .join(name),
        );
    }
    Ok(builder)
}
