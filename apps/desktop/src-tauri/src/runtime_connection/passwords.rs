//! 系统钥匙串适配。应用 identifier 隔离开发与正式条目；远端 origin 作为 account。
#[cfg(target_os = "macos")]
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

pub(super) async fn read(application: String, origin: String) -> Result<Option<String>, ()> {
    tokio::task::spawn_blocking(move || read_sync(&application, &origin))
        .await
        .map_err(|_| ())?
}
pub(super) async fn save(
    application: String,
    origin: String,
    password: String,
    remember: bool,
) -> Result<(), ()> {
    tokio::task::spawn_blocking(move || save_sync(&application, &origin, &password, remember))
        .await
        .map_err(|_| ())?
}
#[cfg(target_os = "macos")]
fn read_sync(application: &str, origin: &str) -> Result<Option<String>, ()> {
    match get_generic_password(&format!("{application}.runtime-password"), origin) {
        Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| ()),
        Err(error) if error.code() == -25300 => Ok(None),
        Err(_) => Err(()),
    }
}
#[cfg(target_os = "macos")]
fn save_sync(application: &str, origin: &str, password: &str, remember: bool) -> Result<(), ()> {
    let service = format!("{application}.runtime-password");
    if remember {
        set_generic_password(&service, origin, password.as_bytes()).map_err(|_| ())
    } else {
        match delete_generic_password(&service, origin) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == -25300 => Ok(()),
            Err(_) => Err(()),
        }
    }
}
#[cfg(not(target_os = "macos"))]
fn read_sync(_: &str, _: &str) -> Result<Option<String>, ()> {
    Ok(None)
}
#[cfg(not(target_os = "macos"))]
fn save_sync(_: &str, _: &str, _: &str, remember: bool) -> Result<(), ()> {
    if remember { Err(()) } else { Ok(()) }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    #[ignore = "显式运行的 macOS 钥匙串检查，只使用独占测试命名空间"]
    fn isolated_keychain_round_trip_and_unreadable_entry() {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).unwrap();
        let suffix: String = bytes.iter().map(|value| format!("{value:02x}")).collect();
        let application = format!("com.ez-assistant.desktop.dev.test-{suffix}");
        let account = "http://runtime-fixture.invalid:1234";
        assert_eq!(read_sync(&application, account).unwrap(), None);
        save_sync(&application, account, "fixture password", true).unwrap();
        assert_eq!(
            read_sync(&application, account).unwrap().as_deref(),
            Some("fixture password")
        );
        assert_eq!(
            read_sync(&application, "http://other-fixture.invalid").unwrap(),
            None
        );
        set_generic_password(&format!("{application}.runtime-password"), account, &[255]).unwrap();
        let invalid = read_sync(&application, account);
        // Always delete our own entry before asserting the error outcome.
        save_sync(&application, account, "", false).unwrap();
        assert!(invalid.is_err());
        assert_eq!(read_sync(&application, account).unwrap(), None);
    }
}
