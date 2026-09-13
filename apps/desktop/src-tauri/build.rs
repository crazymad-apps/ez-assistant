fn main() {
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
