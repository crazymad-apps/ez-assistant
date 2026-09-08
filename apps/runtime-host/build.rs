//! 将显式指定的前端发布目录编入 Host；生成文件只写 OUT_DIR。

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=EZ_ASSISTANT_WEB_DIST");
    let output =
        PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is required")?).join("web_assets.rs");
    let Some(directory) = env::var_os("EZ_ASSISTANT_WEB_DIST").map(PathBuf::from) else {
        if env::var("PROFILE")? == "release" {
            return Err(
                "release Host requires EZ_ASSISTANT_WEB_DIST; build the frontend first".into(),
            );
        }
        fs::write(
            output,
            "pub(super) static WEB_ASSETS: &[(&str, &str, &[u8])] = &[];\n",
        )?;
        return Ok(());
    };
    if !directory.is_absolute() {
        return Err("EZ_ASSISTANT_WEB_DIST must be absolute".into());
    }
    let manifest_path = directory.join("host-web-manifest.json");
    println!("cargo:rerun-if-changed={}", directory.display());
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(manifest_path)?)?;
    if manifest["version"].as_str() != Some(env::var("CARGO_PKG_VERSION")?.as_str()) {
        return Err("frontend and Host product versions differ".into());
    }
    if !directory.join("index.html").is_file() {
        return Err("frontend index.html is missing".into());
    }
    let mut files = Vec::new();
    collect(&directory, &directory, &mut files)?;
    files.sort();
    let mut generated = String::from("pub(super) static WEB_ASSETS: &[(&str, &str, &[u8])] = &[\n");
    for path in files {
        let relative = path
            .strip_prefix(&directory)?
            .to_str()
            .ok_or("non-UTF8 asset path")?;
        if relative != "index.html" && !relative.starts_with("assets/") {
            continue;
        }
        let mime = match path.extension().and_then(|extension| extension.to_str()) {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            Some("svg") => "image/svg+xml",
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("webp") => "image/webp",
            Some("woff2") => "font/woff2",
            Some("woff") => "font/woff",
            Some("ttf") => "font/ttf",
            _ => continue,
        };
        generated.push_str(&format!(
            "({:?}, {:?}, include_bytes!({:?})),\n",
            format!("/{relative}"),
            mime,
            path
        ));
    }
    generated.push_str("];\n");
    fs::write(output, generated)?;
    Ok(())
}

fn collect(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err("frontend assets must not contain symlinks".into());
        }
        if kind.is_dir() {
            // 只索引构建的 assets 子树，不引入源文件、凭据或任意开发目录。
            if directory != root || entry.file_name() == "assets" {
                collect(root, &entry.path(), files)?;
            }
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}
