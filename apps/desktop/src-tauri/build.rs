fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref() == Some("macos") {
        // The development binary embeds this icon through `generate_context!`.
        // Track it explicitly so icon updates cannot reuse a stale debug binary.
        println!("cargo:rerun-if-changed=icons/macos/icon.icns");
    }

    tauri_build::build()
}
