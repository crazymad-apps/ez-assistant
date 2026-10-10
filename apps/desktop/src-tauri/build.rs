fn main() {
    // 公钥在构建时嵌入，WebView 和运行时环境不能替换更新信任根。
    println!("cargo:rerun-if-env-changed=EZ_DESKTOP_UPDATE_PUBLIC_KEY_FILE");
    let key_path = std::env::var_os("EZ_DESKTOP_UPDATE_PUBLIC_KEY_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            "../../../packages/assistant-protocol/resources/desktop-update.pub".into()
        });
    println!("cargo:rerun-if-changed={}", key_path.display());
    let key = match std::fs::read_to_string(&key_path) {
        Ok(key) => {
            assert!(key.len() <= 8192, "update public key is too large");
            key
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && std::env::var_os("EZ_DESKTOP_UPDATE_PUBLIC_KEY_FILE").is_none() =>
        {
            String::new()
        }
        Err(error) => panic!("cannot read update public key: {error}"),
    };
    std::fs::write(
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"))
            .join("desktop-update.pub"),
        key,
    )
    .expect("embed update public key");
    if std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref() == Some("macos") {
        // The development binary embeds this icon through `generate_context!`.
        // Track it explicitly so icon updates cannot reuse a stale debug binary.
        println!("cargo:rerun-if-changed=icons/macos/icon.icns");
    }

    if std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref() == Some("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").ok().as_deref() == Some("msvc")
    {
        // Tauri 的资源仅链接到 bin；库测试也直接导入 TaskDialogIndirect，必须激活
        // Common Controls v6。统一交由链接器给 exe/DLL 嵌入对应 manifest，避免重复资源。
        tauri_build::try_build(
            tauri_build::Attributes::new()
                .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest()),
        )
        .expect("build Tauri resources");
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    } else {
        tauri_build::build();
    }
}
