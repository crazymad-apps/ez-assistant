//! 插件原生配置文件的无格式约束存储；仅管理路径、字节上限和内容摘要。

use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use assistant_protocol::{PluginConfigFileSnapshot, PluginConfigFilesSnapshot};
use sha2::{Digest, Sha256};

pub(crate) const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FILES: usize = 256;
pub(crate) const MCP_OVERRIDES_FILE: &str = "mcp-overrides.json";

pub(crate) fn config_root(user_root: &Path, name: &str) -> io::Result<PathBuf> {
    if !super::plugin_name(name) || name.len() > 64 {
        return Err(invalid());
    }
    let plugins = user_root.join("plugins");
    let plugin = plugins.join(name);
    let config = plugin.join("config");
    for path in [&plugins, &plugin, &config] {
        if !fs::symlink_metadata(path)?.file_type().is_dir() {
            return Err(invalid());
        }
    }
    Ok(config)
}

pub(crate) fn target(root: &Path, relative: &str, create_parents: bool) -> io::Result<PathBuf> {
    // MCP 用户覆盖由正式管理命令解释、校验及 CAS，原生上传不能绕过它。
    if relative.eq_ignore_ascii_case(MCP_OVERRIDES_FILE) {
        return Err(invalid());
    }
    if relative.is_empty()
        || relative.len() > 512
        || relative.contains('\\')
        || relative.contains('\0')
    {
        return Err(invalid());
    }
    let parts = relative.split('/').collect::<Vec<_>>();
    if parts.len() > 8
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == ".." || part.len() > 128)
    {
        return Err(invalid());
    }
    let mut path = root.to_path_buf();
    for part in &parts[..parts.len() - 1] {
        path.push(part);
        if create_parents && !path.exists() {
            fs::create_dir(&path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            }
        }
        if !fs::symlink_metadata(&path)?.file_type().is_dir() {
            return Err(invalid());
        }
    }
    path.push(parts[parts.len() - 1]);
    Ok(path)
}

pub(crate) fn current(path: &Path, relative: &str) -> io::Result<Option<PluginConfigFileSnapshot>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_FILE_BYTES {
        return Err(invalid());
    }
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .filter(|bytes| *bytes <= MAX_FILE_BYTES)
            .ok_or_else(invalid)?;
        hasher.update(&chunk[..read]);
    }
    if bytes != metadata.len() {
        return Err(invalid());
    }
    Ok(Some(PluginConfigFileSnapshot {
        path: relative.to_owned(),
        revision: format!("sha256:{:x}", hasher.finalize()),
        size_bytes: bytes,
    }))
}

pub(crate) fn list(root: &Path) -> io::Result<PluginConfigFilesSnapshot> {
    let mut files = Vec::new();
    visit(root, root, 0, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(PluginConfigFilesSnapshot { files })
}

fn visit(
    root: &Path,
    directory: &Path,
    depth: usize,
    files: &mut Vec<PluginConfigFileSnapshot>,
) -> io::Result<()> {
    if depth >= 8 {
        return Err(invalid());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(MCP_OVERRIDES_FILE)
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_dir() {
            visit(root, &path, depth + 1, files)?;
        } else if metadata.file_type().is_file() {
            if files.len() >= MAX_FILES {
                return Err(invalid());
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| invalid())?
                .to_str()
                .ok_or_else(invalid)?
                .replace(std::path::MAIN_SEPARATOR, "/");
            files.push(current(&path, &relative)?.ok_or_else(invalid)?);
        } else {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    crate::platform::sync_directory(path)
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "plugin configuration path or file is invalid",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_config_metadata_accepts_arbitrary_bytes_and_rejects_link_escape() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(".runtime-test");
        fs::create_dir_all(&base).unwrap();
        let home = tempfile::Builder::new()
            .prefix("c03-m2-config-")
            .tempdir_in(base)
            .unwrap();
        let config = home.path().join("plugins/example-tools/config");
        fs::create_dir_all(&config).unwrap();
        let root = config_root(home.path(), "example-tools").unwrap();
        let file = target(&root, "nested/settings.ini", true).unwrap();
        fs::write(&file, b"token=\xff\0\n").unwrap();
        let listed = list(&root).unwrap();
        assert_eq!(listed.files.len(), 1);
        assert_eq!(listed.files[0].path, "nested/settings.ini");
        assert_eq!(listed.files[0].size_bytes, 9);
        assert!(target(&root, "../data/secret", true).is_err());
        #[cfg(unix)]
        {
            let link = root.join("outside");
            std::os::unix::fs::symlink(home.path(), &link).unwrap();
            assert!(target(&root, "outside/secret", true).is_err());
            assert!(list(&root).is_err());
        }
    }
}
