//! 用户域受管插件目录。目录名仅从已验证的 name、SHA 与操作 key 派生。

use std::{
    io,
    path::{Path, PathBuf},
};

use assistant_protocol::IdempotencyKey;
use sha2::{Digest, Sha256};

use super::manifest::plugin_name;

pub(crate) struct PluginPaths {
    root: PathBuf,
}

impl PluginPaths {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    /// user_root 由 Host 用户域解析，不能由客户端提供；每层既有目录都复核 no-follow。
    pub(crate) fn prepare(user_root: &Path) -> io::Result<Self> {
        let root = user_root.join("plugins");
        crate::config_source::prepare_private_directory(&root).map_err(io::Error::other)?;
        for temporary in [".staging", ".trash"] {
            crate::config_source::prepare_private_directory(&root.join(temporary))
                .map_err(io::Error::other)?;
        }
        Ok(Self { root })
    }

    pub(crate) fn operation(&self, key: &IdempotencyKey, trash: bool) -> PathBuf {
        let digest = Sha256::digest(key.as_str().as_bytes());
        let bucket = if trash { ".trash" } else { ".staging" };
        self.root.join(bucket).join(format!("{digest:x}"))
    }

    pub(crate) fn package(&self, name: &str, sha256: &str) -> io::Result<PathBuf> {
        if !plugin_name(name)
            || name.len() > 64
            || sha256.len() != 64
            || !sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid plugin path component",
            ));
        }
        Ok(self.root.join(name).join("packages").join(sha256))
    }

    pub(crate) fn config(&self, name: &str) -> io::Result<PathBuf> {
        if !plugin_name(name) || name.len() > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid plugin name",
            ));
        }
        Ok(self.root.join(name).join("config"))
    }

    pub(crate) fn data(&self, name: &str) -> io::Result<PathBuf> {
        if !plugin_name(name) || name.len() > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid plugin name",
            ));
        }
        Ok(self.root.join(name).join("data"))
    }

    pub(crate) fn ensure_plugin(&self, name: &str) -> io::Result<()> {
        let config = self.config(name)?;
        let data = self.data(name)?;
        let packages = self.root.join(name).join("packages");
        for directory in [self.root.join(name), config, data, packages] {
            crate::config_source::prepare_private_directory(&directory)
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
}
