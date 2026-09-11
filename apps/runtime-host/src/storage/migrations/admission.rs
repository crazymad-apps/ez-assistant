//! 在 SQLite 可写连接之前核验文件与日志，拒绝可能触发恢复或 sidecar 写入的输入。

use super::{MigrationError, Result};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// SQLite NOFOLLOW 会拒绝祖先目录别名（例如 macOS /var），不只拒绝最终文件。
/// data 目录本身仍须为真实目录；只解析其祖先，不解析最终数据库文件，保留 NOFOLLOW 保护。
pub(super) fn sqlite_path(path: &Path) -> Result<PathBuf> {
    let directory = path.parent().ok_or(MigrationError::InvalidPath)?;
    if !fs::symlink_metadata(directory)?.is_dir() {
        return Err(MigrationError::InvalidPath);
    }
    Ok(directory
        .canonicalize()?
        .join(path.file_name().ok_or(MigrationError::InvalidPath)?))
}

/// 只读预检期间保留打开的文件，后续连接前后核对文件身份与内容变更线索。
/// 不建立磁盘锁、注册表或第二份数据状态；产品实例锁由外层 Host 持有。
pub(super) struct ExistingDatabase {
    file: fs::File,
    metadata: fs::Metadata,
}

impl ExistingDatabase {
    /// NotFound 才是新库；目录别名、非普通文件和不能安全只读的日志状态均失败。
    pub(super) fn inspect(path: &Path) -> Result<Option<Self>> {
        if let Some(parent) = path.parent() {
            match fs::symlink_metadata(parent) {
                Ok(metadata) if !metadata.is_dir() => return Err(MigrationError::InvalidPath),
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.into());
                }
                _ => {}
            }
        }
        check_journals(path)?;
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => return Err(MigrationError::InvalidPath),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(path)?;
        if !same_snapshot(&metadata, &file.metadata()?)? {
            return Err(MigrationError::DatabaseChanged);
        }
        let mut header = [0; 100];
        file.read_exact(&mut header)
            .map_err(|_| MigrationError::Integrity)?;
        if &header[..16] != b"SQLite format 3\0" {
            return Err(MigrationError::Integrity);
        }
        if header[18] != 1 || header[19] != 1 {
            return Err(MigrationError::UnsafeJournal);
        }
        let inspected = Self { file, metadata };
        inspected.check_unchanged(path)?;
        Ok(Some(inspected))
    }

    pub(super) fn check_unchanged(&self, path: &Path) -> Result<()> {
        check_journals(path)?;
        if !same_snapshot(&self.metadata, &fs::symlink_metadata(path)?)?
            || !same_snapshot(&self.metadata, &self.file.metadata()?)?
        {
            return Err(MigrationError::DatabaseChanged);
        }
        Ok(())
    }
}

fn same_snapshot(left: &fs::Metadata, right: &fs::Metadata) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if left.dev() != right.dev() || left.ino() != right.ino() {
            return Ok(false);
        }
    }
    Ok(right.is_file() && left.len() == right.len() && left.modified()? == right.modified()?)
}

fn check_journals(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        match fs::symlink_metadata(Path::new(&name)) {
            // 无锁读取不能可靠判断 journal 是否 hot；保守拒绝所有非空 journal。
            Ok(metadata) if suffix == "-journal" && metadata.is_file() && metadata.len() == 0 => {}
            Ok(_) => return Err(MigrationError::UnsafeJournal),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// 仅目录和符号链接，不创建或打开 SQLite 文件。
    #[test]
    fn sqlite_path_resolves_ancestors_without_following_data_or_database_aliases() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir_all(home.join("data")).unwrap();
        let alias = root.path().join("alias");
        symlink(&home, &alias).unwrap();
        let leaf = alias.join("data/runtime.sqlite3");
        let resolved = sqlite_path(&leaf).unwrap();
        assert_eq!(
            resolved,
            home.canonicalize().unwrap().join("data/runtime.sqlite3")
        );
        symlink(&home, &leaf).unwrap();
        assert_eq!(sqlite_path(&leaf).unwrap(), resolved);
        assert!(matches!(
            ExistingDatabase::inspect(&leaf),
            Err(MigrationError::InvalidPath)
        ));
        let data_alias = root.path().join("data-alias");
        symlink(home.join("data"), &data_alias).unwrap();
        assert!(matches!(
            sqlite_path(&data_alias.join("runtime.sqlite3")),
            Err(MigrationError::InvalidPath)
        ));
    }
}
