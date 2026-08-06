//! Unix Socket endpoint、内核单实例锁与 owned cleanup。

use std::{
    fs::{self, File, OpenOptions, TryLockError},
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use thiserror::Error;
use tokio::net::UnixListener;

const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

#[derive(Debug, Error)]
pub(crate) enum EndpointError {
    #[error("another Runtime process owns the instance lock for endpoint: {path}")]
    AlreadyRunning { path: PathBuf },
    #[error("runtime endpoint exists but is not a Unix socket and was preserved: {path}")]
    UnsafeEndpoint { path: PathBuf },
    #[error("runtime instance lock path is not a regular file and was preserved: {path}")]
    UnsafeLockFile { path: PathBuf },
    #[error("runtime endpoint directory must already be private (0700): {path}")]
    UnsafeDirectory { path: PathBuf },
    #[error("runtime endpoint setup failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// 持有实例锁和本实例成功 bind 的 socket；两者的生命周期都覆盖整个 Host。
pub(crate) struct OwnedEndpoint {
    path: PathBuf,
    identity: FileIdentity,
    listener: UnixListener,
    /// 文件本身允许长期存在；只要句柄存活，内核独占锁就表示本实例仍然存活。
    _instance_lock: File,
}

#[derive(Clone, Copy)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl OwnedEndpoint {
    pub(crate) fn bind(path: PathBuf) -> Result<Self, EndpointError> {
        prepare_parent(&path)?;
        let instance_lock = acquire_instance_lock(&path)?;
        remove_stale_socket(&path)?;

        let listener = UnixListener::bind(&path).map_err(|source| EndpointError::Io {
            path: path.clone(),
            source,
        })?;
        set_private_permissions(&path)?;
        let metadata = fs::symlink_metadata(&path).map_err(|source| EndpointError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(Self {
            path,
            identity: FileIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            listener,
            _instance_lock: instance_lock,
        })
    }

    pub(crate) fn listener(&self) -> &UnixListener {
        &self.listener
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for OwnedEndpoint {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.dev() == self.identity.device && metadata.ino() == self.identity.inode {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn prepare_parent(path: &Path) -> Result<(), EndpointError> {
    let parent = path.parent().ok_or_else(|| EndpointError::Io {
        path: path.to_owned(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "endpoint must have a parent directory",
        ),
    })?;
    match fs::create_dir(parent) {
        Ok(()) => fs::set_permissions(parent, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
            .map_err(|source| EndpointError::Io {
                path: parent.to_owned(),
                source,
            }),
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::metadata(parent).map_err(|source| EndpointError::Io {
                path: parent.to_owned(),
                source,
            })?;
            if metadata.is_dir() && metadata.permissions().mode() & 0o777 == PRIVATE_DIRECTORY_MODE
            {
                Ok(())
            } else {
                Err(EndpointError::UnsafeDirectory {
                    path: parent.to_owned(),
                })
            }
        }
        Err(source) => Err(EndpointError::Io {
            path: parent.to_owned(),
            source,
        }),
    }
}

fn acquire_instance_lock(endpoint_path: &Path) -> Result<File, EndpointError> {
    let path = instance_lock_path(endpoint_path);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(EndpointError::UnsafeLockFile { path });
        }
        Ok(_) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(EndpointError::Io { path, source }),
    }

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| EndpointError::Io {
            path: path.clone(),
            source,
        })?;
    set_private_permissions(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(EndpointError::AlreadyRunning {
            path: endpoint_path.to_owned(),
        }),
        Err(TryLockError::Error(source)) => Err(EndpointError::Io { path, source }),
    }
}

fn instance_lock_path(endpoint_path: &Path) -> PathBuf {
    endpoint_path.with_extension("lock")
}

fn remove_stale_socket(path: &Path) -> Result<(), EndpointError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            fs::remove_file(path).map_err(|source| EndpointError::Io {
                path: path.to_owned(),
                source,
            })
        }
        Ok(_) => Err(EndpointError::UnsafeEndpoint {
            path: path.to_owned(),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(EndpointError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

fn set_private_permissions(path: &Path) -> Result<(), EndpointError> {
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE)).map_err(|source| {
        EndpointError::Io {
            path: path.to_owned(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::{fs::PermissionsExt, net::UnixListener as StdUnixListener};

    use tempfile::tempdir;

    use super::*;

    fn private_directory(root: &Path) -> PathBuf {
        let path = root.join("runtime");
        fs::create_dir(&path).expect("private directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
            .expect("private mode");
        path
    }

    #[tokio::test]
    async fn bind_sets_private_modes_and_drop_removes_only_socket() {
        let directory = tempdir().expect("tempdir");
        let runtime_dir = directory.path().join("runtime");
        let path = runtime_dir.join("runtime.sock");
        let endpoint = OwnedEndpoint::bind(path.clone()).expect("bind");
        let lock_path = instance_lock_path(&path);
        assert_eq!(
            fs::metadata(&runtime_dir)
                .expect("runtime dir")
                .permissions()
                .mode()
                & 0o777,
            PRIVATE_DIRECTORY_MODE
        );
        assert_eq!(
            fs::metadata(&path).expect("socket").permissions().mode() & 0o777,
            PRIVATE_FILE_MODE
        );
        assert_eq!(
            fs::metadata(&lock_path)
                .expect("lock file")
                .permissions()
                .mode()
                & 0o777,
            PRIVATE_FILE_MODE
        );
        drop(endpoint);
        assert!(!path.exists());
        assert!(lock_path.exists(), "stable lock file remains reusable");

        let rebound = OwnedEndpoint::bind(path.clone()).expect("released lock can be acquired");
        drop(rebound);
    }

    #[tokio::test]
    async fn held_kernel_lock_rejects_a_second_instance_without_probing_socket() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("runtime").join("runtime.sock");
        let endpoint = OwnedEndpoint::bind(path.clone()).expect("first bind");

        assert!(matches!(
            OwnedEndpoint::bind(path.clone()),
            Err(EndpointError::AlreadyRunning { path: actual }) if actual == path
        ));
        drop(endpoint);
    }

    #[tokio::test]
    async fn stale_unix_socket_is_replaced_after_kernel_lock_is_acquired() {
        let directory = tempdir().expect("tempdir");
        let path = private_directory(directory.path()).join("runtime.sock");
        let stale = StdUnixListener::bind(&path).expect("stale listener");
        drop(stale);
        let stale_identity = fs::symlink_metadata(&path).expect("stale socket").ino();

        let endpoint = OwnedEndpoint::bind(path.clone()).expect("replace stale socket");
        let current_identity = fs::symlink_metadata(&path).expect("current socket").ino();
        assert_ne!(current_identity, stale_identity);
        drop(endpoint);
    }

    #[test]
    fn non_socket_endpoint_is_preserved() {
        let directory = tempdir().expect("tempdir");
        let path = private_directory(directory.path()).join("runtime.sock");
        fs::write(&path, b"not a socket").expect("conflicting file");
        assert!(matches!(
            OwnedEndpoint::bind(path.clone()),
            Err(EndpointError::UnsafeEndpoint { path: actual }) if actual == path
        ));
        assert_eq!(
            fs::read(&path).expect("conflicting file preserved"),
            b"not a socket"
        );
    }

    #[test]
    fn non_regular_lock_path_is_preserved() {
        let directory = tempdir().expect("tempdir");
        let runtime_dir = private_directory(directory.path());
        let path = runtime_dir.join("runtime.sock");
        let lock_path = instance_lock_path(&path);
        fs::create_dir(&lock_path).expect("conflicting lock directory");

        assert!(matches!(
            OwnedEndpoint::bind(path),
            Err(EndpointError::UnsafeLockFile { path: actual }) if actual == lock_path
        ));
        assert!(lock_path.is_dir());
    }

    #[tokio::test]
    async fn cleanup_does_not_remove_a_replaced_path() {
        let directory = tempdir().expect("tempdir");
        let path = private_directory(directory.path()).join("runtime.sock");
        let endpoint = OwnedEndpoint::bind(path.clone()).expect("bind");
        fs::remove_file(&path).expect("unlink owned socket");
        fs::write(&path, b"replacement").expect("replacement");
        drop(endpoint);
        assert_eq!(
            fs::read(&path).expect("replacement remains"),
            b"replacement"
        );
    }

    #[test]
    fn bind_never_changes_permissions_of_an_existing_shared_directory() {
        let directory = tempdir().expect("tempdir");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755))
            .expect("shared mode");
        let path = directory.path().join("runtime.sock");
        assert!(matches!(
            OwnedEndpoint::bind(path),
            Err(EndpointError::UnsafeDirectory { .. })
        ));
        assert_eq!(
            fs::metadata(directory.path())
                .expect("directory")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
}
