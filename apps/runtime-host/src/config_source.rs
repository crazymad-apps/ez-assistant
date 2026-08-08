//! Unix Runtime Home 建立与私有 `config.toml` 安全读取。

use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use assistant_runtime::{
    ConfigSourceFailure, ConfigSourceFailureKind, ConfigSourceFuture, ConfigSourceLoad,
    RuntimeConfigSource,
};
use thiserror::Error;

const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_CONFIG_MODE: u32 = 0o600;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const RUN_DIRECTORY: &str = "run";

#[derive(Debug, Error)]
pub(crate) enum RuntimeHomeError {
    #[error("Runtime directory cannot be secured as a private directory: {path}")]
    Unsafe { path: PathBuf },
    #[error("Runtime Home setup failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// 创建 Runtime Home 和 `run/`，并通过目录句柄把权限统一收紧为 0700。
pub(crate) fn prepare_runtime_home(path: &Path) -> Result<(), RuntimeHomeError> {
    prepare_private_directory(path)?;
    prepare_private_directory(&path.join(RUN_DIRECTORY))
}

pub(crate) fn prepare_private_directory(path: &Path) -> Result<(), RuntimeHomeError> {
    let initial = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|source| RuntimeHomeError::Io {
                path: path.to_owned(),
                source,
            })?;
            fs::symlink_metadata(path).map_err(|source| RuntimeHomeError::Io {
                path: path.to_owned(),
                source,
            })?
        }
        Err(source) => {
            return Err(RuntimeHomeError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    if !initial.file_type().is_dir() {
        return Err(RuntimeHomeError::Unsafe {
            path: path.to_owned(),
        });
    }

    // 通过目录 fd 修改权限，避免检查后路径被替换为 symlink 时 chmod 到其他目标。
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| RuntimeHomeError::Io {
            path: path.to_owned(),
            source,
        })?;
    let opened = directory
        .metadata()
        .map_err(|source| RuntimeHomeError::Io {
            path: path.to_owned(),
            source,
        })?;
    if !opened.file_type().is_dir()
        || initial.dev() != opened.dev()
        || initial.ino() != opened.ino()
    {
        return Err(RuntimeHomeError::Unsafe {
            path: path.to_owned(),
        });
    }
    directory
        .set_permissions(fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
        .map_err(|source| RuntimeHomeError::Io {
            path: path.to_owned(),
            source,
        })?;
    let secured = directory
        .metadata()
        .map_err(|source| RuntimeHomeError::Io {
            path: path.to_owned(),
            source,
        })?;
    if secured.permissions().mode() & 0o777 != PRIVATE_DIRECTORY_MODE {
        return Err(RuntimeHomeError::Unsafe {
            path: path.to_owned(),
        });
    }
    Ok(())
}

/// 生产 Host 使用的单一配置文件来源。
pub(crate) struct LocalConfigSource {
    path: PathBuf,
}

impl LocalConfigSource {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl RuntimeConfigSource for LocalConfigSource {
    fn display_path(&self) -> Option<String> {
        Some(self.path.to_string_lossy().into_owned())
    }

    fn load(&self) -> ConfigSourceFuture<'_> {
        let path = self.path.clone();
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || read_private_config(&path)).await {
                Ok(result) => result,
                Err(_) => unavailable(
                    ConfigSourceFailureKind::Read,
                    "configuration read task failed",
                ),
            }
        })
    }
}

fn read_private_config(path: &Path) -> ConfigSourceLoad {
    let initial = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return ConfigSourceLoad::Missing;
        }
        Err(_) => {
            return unavailable(
                ConfigSourceFailureKind::Read,
                "configuration file metadata could not be read",
            );
        }
    };
    if !initial.file_type().is_file() {
        return unavailable(
            ConfigSourceFailureKind::Unsafe,
            "configuration file must be a regular file",
        );
    }
    if initial.len() > MAX_CONFIG_BYTES {
        return unavailable(
            ConfigSourceFailureKind::Unsafe,
            "configuration file exceeds the size limit",
        );
    }

    // O_NOFOLLOW closes the symlink swap window between metadata inspection and open.
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(_) => {
            return unavailable(
                ConfigSourceFailureKind::Read,
                "configuration file could not be opened safely",
            );
        }
    };
    let opened = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => {
            return unavailable(
                ConfigSourceFailureKind::Read,
                "configuration file metadata changed while opening",
            );
        }
    };
    if !opened.file_type().is_file()
        || initial.dev() != opened.dev()
        || initial.ino() != opened.ino()
    {
        return unavailable(
            ConfigSourceFailureKind::Unsafe,
            "configuration file changed during its safety check",
        );
    }
    if opened.len() > MAX_CONFIG_BYTES {
        return unavailable(
            ConfigSourceFailureKind::Unsafe,
            "configuration file exceeds the size limit",
        );
    }
    if opened.permissions().mode() & 0o777 != PRIVATE_CONFIG_MODE
        && file
            .set_permissions(fs::Permissions::from_mode(PRIVATE_CONFIG_MODE))
            .is_err()
    {
        return unavailable(
            ConfigSourceFailureKind::Unsafe,
            "configuration file permissions could not be secured",
        );
    }
    let secured = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => {
            return unavailable(
                ConfigSourceFailureKind::Read,
                "configuration file metadata could not be verified",
            );
        }
    };
    if !secured.file_type().is_file() || secured.permissions().mode() & 0o777 != PRIVATE_CONFIG_MODE
    {
        return unavailable(
            ConfigSourceFailureKind::Unsafe,
            "configuration file permissions are not private",
        );
    }

    let mut bytes = Vec::new();
    if file
        .by_ref()
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return unavailable(
            ConfigSourceFailureKind::Read,
            "configuration file could not be read",
        );
    }
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return unavailable(
            ConfigSourceFailureKind::Unsafe,
            "configuration file exceeds the size limit",
        );
    }
    match String::from_utf8(bytes) {
        Ok(document) => ConfigSourceLoad::Document(document),
        Err(_) => unavailable(
            ConfigSourceFailureKind::Read,
            "configuration file is not valid UTF-8",
        ),
    }
}

fn unavailable(kind: ConfigSourceFailureKind, message: &'static str) -> ConfigSourceLoad {
    ConfigSourceLoad::Unavailable(ConfigSourceFailure::new(kind, message))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn creates_and_normalizes_private_runtime_directories() {
        let directory = tempdir().expect("tempdir");
        let home = directory.path().join("runtime");
        prepare_runtime_home(&home).expect("prepare");
        assert_eq!(
            fs::metadata(&home).expect("metadata").permissions().mode() & 0o777,
            PRIVATE_DIRECTORY_MODE
        );
        assert_eq!(
            fs::metadata(home.join(RUN_DIRECTORY))
                .expect("run metadata")
                .permissions()
                .mode()
                & 0o777,
            PRIVATE_DIRECTORY_MODE
        );

        fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).expect("permissions");
        fs::set_permissions(home.join(RUN_DIRECTORY), fs::Permissions::from_mode(0o755))
            .expect("run permissions");
        prepare_runtime_home(&home).expect("normalize");
        assert_eq!(
            fs::metadata(&home).expect("metadata").permissions().mode() & 0o777,
            PRIVATE_DIRECTORY_MODE
        );
        assert_eq!(
            fs::metadata(home.join(RUN_DIRECTORY))
                .expect("run metadata")
                .permissions()
                .mode()
                & 0o777,
            PRIVATE_DIRECTORY_MODE
        );

        let target = directory.path().join("other-runtime");
        fs::create_dir(&target).expect("target directory");
        fs::set_permissions(&target, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
            .expect("target permissions");
        let linked_home = directory.path().join("linked-runtime");
        symlink(&target, &linked_home).expect("runtime symlink");
        assert!(matches!(
            prepare_runtime_home(&linked_home),
            Err(RuntimeHomeError::Unsafe { .. })
        ));
    }

    #[tokio::test]
    async fn missing_and_private_config_are_loaded_without_exposing_file_details() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("config.toml");
        let source = LocalConfigSource::new(path.clone());
        assert!(matches!(source.load().await, ConfigSourceLoad::Missing));

        fs::write(&path, "version = 1\n").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(PRIVATE_CONFIG_MODE))
            .expect("permissions");
        let ConfigSourceLoad::Document(document) = source.load().await else {
            panic!("document");
        };
        assert_eq!(document, "version = 1\n");
    }

    #[tokio::test]
    async fn normalizes_broad_mode_and_rejects_symlink_non_file_and_oversized_file() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("config.toml");
        fs::write(&path, "secret").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("permissions");
        let ConfigSourceLoad::Document(document) =
            LocalConfigSource::new(path.clone()).load().await
        else {
            panic!("normalized document");
        };
        assert_eq!(document, "secret");
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            PRIVATE_CONFIG_MODE
        );

        fs::remove_file(&path).expect("remove");
        let target = directory.path().join("target.toml");
        fs::write(&target, "secret").expect("target");
        fs::set_permissions(&target, fs::Permissions::from_mode(PRIVATE_CONFIG_MODE))
            .expect("permissions");
        symlink(&target, &path).expect("symlink");
        assert_unsafe(LocalConfigSource::new(path.clone()).load().await);

        fs::remove_file(&path).expect("remove symlink");
        fs::create_dir(&path).expect("directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(PRIVATE_CONFIG_MODE))
            .expect("permissions");
        assert_unsafe(LocalConfigSource::new(path.clone()).load().await);

        fs::remove_dir(&path).expect("remove directory");
        let file = fs::File::create(&path).expect("create");
        file.set_len(MAX_CONFIG_BYTES + 1).expect("size");
        fs::set_permissions(&path, fs::Permissions::from_mode(PRIVATE_CONFIG_MODE))
            .expect("permissions");
        assert_unsafe(LocalConfigSource::new(path).load().await);
    }

    fn assert_unsafe(load: ConfigSourceLoad) {
        let ConfigSourceLoad::Unavailable(failure) = load else {
            panic!("unavailable");
        };
        assert_eq!(failure.kind(), ConfigSourceFailureKind::Unsafe);
    }
}
