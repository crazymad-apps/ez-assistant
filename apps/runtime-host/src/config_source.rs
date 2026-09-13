//! Runtime Home 建立与私有 `config.toml` 安全读取。
//!
//! 平台差异（mode/属主/O_NOFOLLOW/目录 fsync 与 Windows 等价物）收敛在
//! [`crate::platform`]；本模块只保留业务语义：单一配置文件、CAS 替换与上限校验。

use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use assistant_runtime::{
    ConfigDocument, ConfigSourceFailure, ConfigSourceFailureKind, ConfigSourceFuture,
    ConfigSourceLoad, ConfigSourceReplace, ConfigSourceReplaceFuture, RuntimeConfigSource,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::platform;

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
    if !initial.file_type().is_dir()
        || platform::is_reparse_or_link(&initial)
        || !platform::owned_by_current_user(path, &initial)
    {
        return Err(RuntimeHomeError::Unsafe {
            path: path.to_owned(),
        });
    }
    // Unix：通过目录 fd 收紧 0700 并复核；Windows：目录安全目标已在上方核验。
    platform::tighten_private_directory(path, &initial).map_err(|source| RuntimeHomeError::Io {
        path: path.to_owned(),
        source,
    })?;
    Ok(())
}

/// 生产 Host 使用的单一配置文件来源。
pub(crate) struct LocalConfigSource {
    path: PathBuf,
    repair_permissions: bool,
    /// 同一来源的 Runtime／Speech／Host 设置串行提交，避免两个 CAS 同时通过后互相覆盖。
    write_gate: std::sync::Arc<tokio::sync::Mutex<()>>,
}

impl LocalConfigSource {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            repair_permissions: true,
            write_gate: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

impl LocalConfigSource {
    pub(crate) fn read_only(path: PathBuf) -> Self {
        Self {
            repair_permissions: false,
            ..Self::new(path)
        }
    }
}

impl RuntimeConfigSource for LocalConfigSource {
    fn display_path(&self) -> Option<String> {
        Some(self.path.to_string_lossy().into_owned())
    }

    fn load(&self) -> ConfigSourceFuture<'_> {
        let path = self.path.clone();
        let repair = self.repair_permissions;
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || read_config(&path, repair)).await {
                Ok(result) => result,
                Err(_) => unavailable(
                    ConfigSourceFailureKind::Read,
                    "configuration read task failed",
                ),
            }
        })
    }

    fn replace(
        &self,
        expected_revision: Option<String>,
        document: String,
    ) -> ConfigSourceReplaceFuture<'_> {
        let path = self.path.clone();
        Box::pin(async move {
            if !self.repair_permissions {
                return ConfigSourceReplace::Unavailable(ConfigSourceFailure::new(
                    ConfigSourceFailureKind::Unsafe,
                    "read-only configuration source",
                ));
            }
            let write_guard = self.write_gate.clone().lock_owned().await;
            match tokio::task::spawn_blocking(move || {
                let _write_guard = write_guard;
                replace_private_config(&path, expected_revision.as_deref(), &document)
            })
            .await
            {
                Ok(result) => result,
                Err(_) => ConfigSourceReplace::Unavailable(ConfigSourceFailure::new(
                    ConfigSourceFailureKind::Read,
                    "configuration write task failed",
                )),
            }
        })
    }
}

fn read_private_config(path: &Path) -> ConfigSourceLoad {
    read_config(path, true)
}

fn read_config(path: &Path, repair: bool) -> ConfigSourceLoad {
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
    if !initial.file_type().is_file()
        || platform::is_reparse_or_link(&initial)
        || !platform::owned_by_current_user(path, &initial)
    {
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

    // 平台原语附加 O_NOFOLLOW 等打开防护，关闭检查与 open 之间的替换窗口。
    let mut options = OpenOptions::new();
    options.read(true);
    platform::apply_read_no_follow(&mut options);
    let mut file = match options.open(path) {
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
    if !opened.file_type().is_file() || !platform::same_file_identity(&initial, &opened) {
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
    if !repair && !platform::private_file_mode_is_secured(&opened) {
        return unavailable(
            ConfigSourceFailureKind::Unsafe,
            "configuration file permissions are not private",
        );
    }
    if repair
        && !platform::private_file_mode_is_exact(&opened)
        && platform::tighten_private_file(&file).is_err()
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
    if !secured.file_type().is_file() || !platform::private_file_mode_is_secured(&secured) {
        return unavailable(
            ConfigSourceFailureKind::Unsafe,
            "configuration file permissions are not private",
        );
    }

    let mut bytes = Vec::new();
    if Read::by_ref(&mut file)
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
        Ok(document) => {
            let revision = configuration_revision(document.as_bytes());
            ConfigSourceLoad::Document(ConfigDocument::new(document, revision))
        }
        Err(_) => unavailable(
            ConfigSourceFailureKind::Read,
            "configuration file is not valid UTF-8",
        ),
    }
}

fn replace_private_config(
    path: &Path,
    expected_revision: Option<&str>,
    document: &str,
) -> ConfigSourceReplace {
    if document.len() as u64 > MAX_CONFIG_BYTES {
        return ConfigSourceReplace::Unavailable(ConfigSourceFailure::new(
            ConfigSourceFailureKind::Unsafe,
            "configuration candidate exceeds the size limit",
        ));
    }

    let current = read_private_config(path);
    let current_matches = match &current {
        ConfigSourceLoad::Missing => expected_revision.is_none(),
        ConfigSourceLoad::Document(current) => Some(current.revision()) == expected_revision,
        ConfigSourceLoad::Unavailable(failure) => {
            return ConfigSourceReplace::Unavailable(*failure);
        }
    };
    if !current_matches {
        return ConfigSourceReplace::Conflict(current);
    }

    let parent = match path.parent() {
        Some(parent) => parent,
        None => {
            return ConfigSourceReplace::Unavailable(ConfigSourceFailure::new(
                ConfigSourceFailureKind::Read,
                "configuration parent directory is unavailable",
            ));
        }
    };
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp_path = parent.join(format!(".config.toml.{}.{}.tmp", std::process::id(), nonce));
    let write_result = (|| -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        platform::apply_private_open_options(&mut options);
        let mut temp = options.open(&temp_path)?;
        temp.write_all(document.as_bytes())?;
        temp.sync_all()?;

        let latest = read_private_config(path);
        let still_matches = match latest {
            ConfigSourceLoad::Missing => expected_revision.is_none(),
            ConfigSourceLoad::Document(latest) => Some(latest.revision()) == expected_revision,
            ConfigSourceLoad::Unavailable(_) => false,
        };
        if !still_matches {
            return Err(std::io::Error::other("configuration revision changed"));
        }

        fs::rename(&temp_path, path)?;
        // Unix：父目录 fsync；Windows：rename 后重读内容校验。
        platform::sync_directory(parent)?;
        platform::verify_replacement_contents(path, document.as_bytes())?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
        let latest = read_private_config(path);
        let conflict = match &latest {
            ConfigSourceLoad::Missing => expected_revision.is_some(),
            ConfigSourceLoad::Document(latest) => Some(latest.revision()) != expected_revision,
            ConfigSourceLoad::Unavailable(_) => false,
        };
        return if conflict {
            ConfigSourceReplace::Conflict(latest)
        } else {
            ConfigSourceReplace::Unavailable(ConfigSourceFailure::new(
                ConfigSourceFailureKind::Read,
                "configuration file could not be replaced",
            ))
        };
    }

    match read_private_config(path) {
        ConfigSourceLoad::Document(document) => ConfigSourceReplace::Applied(document),
        ConfigSourceLoad::Missing => ConfigSourceReplace::Unavailable(ConfigSourceFailure::new(
            ConfigSourceFailureKind::Read,
            "configuration file disappeared after replacement",
        )),
        ConfigSourceLoad::Unavailable(failure) => ConfigSourceReplace::Unavailable(failure),
    }
}

fn configuration_revision(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

fn unavailable(kind: ConfigSourceFailureKind, message: &'static str) -> ConfigSourceLoad {
    ConfigSourceLoad::Unavailable(ConfigSourceFailure::new(kind, message))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::tempdir;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn creates_and_normalizes_private_runtime_directories() {
        let directory = tempdir().expect("tempdir");
        let home = directory.path().join("runtime");
        prepare_runtime_home(&home).expect("prepare");
        assert_eq!(
            fs::metadata(&home).expect("metadata").permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(home.join(RUN_DIRECTORY))
                .expect("run metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).expect("permissions");
        fs::set_permissions(home.join(RUN_DIRECTORY), fs::Permissions::from_mode(0o755))
            .expect("run permissions");
        prepare_runtime_home(&home).expect("normalize");
        assert_eq!(
            fs::metadata(&home).expect("metadata").permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(home.join(RUN_DIRECTORY))
                .expect("run metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        let target = directory.path().join("other-runtime");
        fs::create_dir(&target).expect("target directory");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700))
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
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");
        let ConfigSourceLoad::Document(document) = source.load().await else {
            panic!("document");
        };
        assert_eq!(document.contents(), "version = 1\n");
    }

    #[cfg(unix)]
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
        assert_eq!(document.contents(), "secret");
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );

        fs::remove_file(&path).expect("remove");
        let target = directory.path().join("target.toml");
        fs::write(&target, "secret").expect("target");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("permissions");
        symlink(&target, &path).expect("symlink");
        assert_unsafe(LocalConfigSource::new(path.clone()).load().await);

        fs::remove_file(&path).expect("remove symlink");
        fs::create_dir(&path).expect("directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");
        assert_unsafe(LocalConfigSource::new(path.clone()).load().await);

        fs::remove_dir(&path).expect("remove directory");
        let file = fs::File::create(&path).expect("create");
        file.set_len(MAX_CONFIG_BYTES + 1).expect("size");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");
        assert_unsafe(LocalConfigSource::new(path.clone()).load().await);
    }

    #[tokio::test]
    async fn replaces_config_with_revision_cas_and_private_permissions() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("config.toml");
        let source = LocalConfigSource::new(path.clone());

        let ConfigSourceReplace::Applied(created) = source
            .replace(None, "schema_version = 1\n".to_owned())
            .await
        else {
            panic!("created");
        };
        assert_eq!(created.contents(), "schema_version = 1\n");
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );

        assert!(matches!(
            source
                .replace(Some("stale".to_owned()), "other = true\n".to_owned())
                .await,
            ConfigSourceReplace::Conflict(_)
        ));
        let ConfigSourceReplace::Applied(updated) = source
            .replace(
                Some(created.revision().to_owned()),
                "schema_version = 2\n".to_owned(),
            )
            .await
        else {
            panic!("updated");
        };
        assert_eq!(updated.contents(), "schema_version = 2\n");
    }

    #[tokio::test]
    async fn concurrent_writers_with_the_same_revision_cannot_both_commit() {
        let directory = tempdir().expect("tempdir");
        let source = LocalConfigSource::new(directory.path().join("config.toml"));
        let (first, second) = tokio::join!(
            source.replace(None, "owner = 'first'\n".into()),
            source.replace(None, "owner = 'second'\n".into()),
        );
        assert_eq!(
            usize::from(matches!(first, ConfigSourceReplace::Applied(_)))
                + usize::from(matches!(second, ConfigSourceReplace::Applied(_))),
            1
        );
        assert_eq!(
            usize::from(matches!(first, ConfigSourceReplace::Conflict(_)))
                + usize::from(matches!(second, ConfigSourceReplace::Conflict(_))),
            1
        );
    }

    #[cfg(unix)]
    fn assert_unsafe(load: ConfigSourceLoad) {
        let ConfigSourceLoad::Unavailable(failure) = load else {
            panic!("unavailable");
        };
        assert_eq!(failure.kind(), ConfigSourceFailureKind::Unsafe);
    }
}
