//! 共享发布声明只读取根 manifest，不读取任何 Client 工程或安装产物。

#[path = "src/software_version.rs"]
mod software_version;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed=src/software_version.rs");
    let document: toml::Value = toml::from_str(&std::fs::read_to_string(manifest)?)
        .map_err(|_| "invalid workspace manifest")?;
    let workspace = document.get("workspace").ok_or("missing workspace")?;
    let version = workspace
        .get("package")
        .and_then(|value| value.get("version"))
        .and_then(toml::Value::as_str)
        .ok_or("missing software version")?;
    let minimum = workspace
        .get("metadata")
        .and_then(|value| value.get("ez-assistant"))
        .and_then(|value| value.get("min_compatible_version"))
        .and_then(toml::Value::as_str)
        .ok_or("missing minimum compatible software version")?;
    let parsed =
        software_version::parse_software_version(version).ok_or("invalid software version")?;
    let lower = software_version::parse_software_version(minimum)
        .ok_or("invalid minimum compatible version")?;
    if lower > parsed || version != env!("CARGO_PKG_VERSION") {
        return Err("inconsistent software release declaration".into());
    }
    println!("cargo:rustc-env=EZ_ASSISTANT_MIN_COMPATIBLE_VERSION={minimum}");
    Ok(())
}
