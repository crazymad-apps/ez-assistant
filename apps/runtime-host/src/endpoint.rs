//! Runtime 单实例锁、动态 loopback 监听与私有发现文件所有权。

use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::Write,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;

const RUN_DIRECTORY: &str = "run";
const LOCK_FILE: &str = "runtime.lock";
const DISCOVERY_FILE: &str = "runtime.json";
const PRIVATE_FILE_MODE: u32 = 0o600;
const TOKEN_BYTES: usize = 32;
const INSTANCE_ID_BYTES: usize = 16;

#[derive(Debug, Error)]
pub(crate) enum EndpointError {
    #[error("another Runtime process owns the instance lock: {path}")]
    AlreadyRunning { path: PathBuf },
    #[error("runtime instance lock path is not a regular file and was preserved: {path}")]
    UnsafeLockFile { path: PathBuf },
    #[error("runtime discovery path is not a regular file and was preserved: {path}")]
    UnsafeDiscoveryFile { path: PathBuf },
    #[error("runtime endpoint setup failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("runtime endpoint secret generation failed")]
    Entropy(#[from] getrandom::Error),
    #[error("runtime discovery data could not be encoded")]
    DiscoveryEncoding(#[from] serde_json::Error),
}

/// 在打开 Store 前取得的进程级所有权，防止两个 Host 同时使用同一 Runtime Home。
pub(crate) struct RuntimeInstanceGuard {
    run_directory: PathBuf,
    instance_lock: File,
}

/// 已发布的本地 HTTP endpoint；Drop 只清理仍属于本实例的发现文件。
pub(crate) struct OwnedEndpoint {
    listener: Option<TcpListener>,
    discovery_path: PathBuf,
    instance_id: String,
    access_token: String,
    address: SocketAddr,
    _instance_lock: File,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct RuntimeDiscovery {
    pub(crate) address: String,
    pub(crate) instance_id: String,
    pub(crate) access_token: String,
    pub(crate) pid: u32,
}

impl RuntimeInstanceGuard {
    /// 获取 Runtime Home 对应的稳定内核锁。
    pub(crate) fn acquire(runtime_home: &Path) -> Result<Self, EndpointError> {
        let run_directory = runtime_home.join(RUN_DIRECTORY);
        let lock_path = run_directory.join(LOCK_FILE);
        validate_regular_file_or_missing(&lock_path, true)?;
        let instance_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)
            .map_err(|source| EndpointError::Io {
                path: lock_path.clone(),
                source,
            })?;
        instance_lock
            .set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
            .map_err(|source| EndpointError::Io {
                path: lock_path.clone(),
                source,
            })?;
        match instance_lock.try_lock() {
            Ok(()) => Ok(Self {
                run_directory,
                instance_lock,
            }),
            Err(TryLockError::WouldBlock) => Err(EndpointError::AlreadyRunning { path: lock_path }),
            Err(TryLockError::Error(source)) => Err(EndpointError::Io {
                path: lock_path,
                source,
            }),
        }
    }

    /// 在 Runtime 恢复完成后绑定动态端口并原子发布发现信息。
    pub(crate) async fn bind_and_publish(self) -> Result<OwnedEndpoint, EndpointError> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|source| EndpointError::Io {
                path: self.run_directory.clone(),
                source,
            })?;
        let address = listener.local_addr().map_err(|source| EndpointError::Io {
            path: self.run_directory.clone(),
            source,
        })?;
        let instance_id = random_secret(INSTANCE_ID_BYTES)?;
        let access_token = random_secret(TOKEN_BYTES)?;
        let discovery_path = self.run_directory.join(DISCOVERY_FILE);
        validate_regular_file_or_missing(&discovery_path, false)?;
        let discovery = RuntimeDiscovery {
            address: format!("http://{address}"),
            instance_id: instance_id.clone(),
            access_token: access_token.clone(),
            pid: std::process::id(),
        };
        write_discovery_atomic(&self.run_directory, &discovery_path, &discovery)?;
        Ok(OwnedEndpoint {
            listener: Some(listener),
            discovery_path,
            instance_id,
            access_token,
            address,
            _instance_lock: self.instance_lock,
        })
    }
}

impl OwnedEndpoint {
    pub(crate) fn take_listener(&mut self) -> TcpListener {
        self.listener
            .take()
            .expect("RuntimeServer takes the endpoint listener exactly once")
    }

    #[cfg(test)]
    pub(crate) fn address(&self) -> SocketAddr {
        self.address
    }

    pub(crate) fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub(crate) fn authority(&self) -> String {
        self.address.to_string()
    }

    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }

    pub(crate) fn discovery_path(&self) -> &Path {
        &self.discovery_path
    }
}

impl Drop for OwnedEndpoint {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.discovery_path) else {
            return;
        };
        if !metadata.file_type().is_file() {
            return;
        }
        let Ok(bytes) = fs::read(&self.discovery_path) else {
            return;
        };
        let Ok(discovery) = serde_json::from_slice::<RuntimeDiscovery>(&bytes) else {
            return;
        };
        if discovery.instance_id != self.instance_id {
            return;
        }
        if fs::remove_file(&self.discovery_path).is_ok()
            && let Some(parent) = self.discovery_path.parent()
        {
            let _ = sync_directory(parent);
        }
    }
}

fn random_secret(length: usize) -> Result<String, getrandom::Error> {
    let mut bytes = vec![0_u8; length];
    getrandom::fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn validate_regular_file_or_missing(path: &Path, is_lock: bool) -> Result<(), EndpointError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) if is_lock => Err(EndpointError::UnsafeLockFile {
            path: path.to_owned(),
        }),
        Ok(_) => Err(EndpointError::UnsafeDiscoveryFile {
            path: path.to_owned(),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(EndpointError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

fn write_discovery_atomic(
    run_directory: &Path,
    discovery_path: &Path,
    discovery: &RuntimeDiscovery,
) -> Result<(), EndpointError> {
    let bytes = serde_json::to_vec_pretty(discovery)?;
    let staging_path = run_directory.join(format!(".runtime-{}.tmp", discovery.instance_id));
    let mut staging = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&staging_path)
        .map_err(|source| EndpointError::Io {
            path: staging_path.clone(),
            source,
        })?;
    let staging_result = (|| {
        staging.write_all(&bytes)?;
        staging.sync_all()?;
        fs::rename(&staging_path, discovery_path)
    })();
    if let Err(source) = staging_result {
        let _ = fs::remove_file(&staging_path);
        return Err(EndpointError::Io {
            path: discovery_path.to_owned(),
            source,
        });
    }
    if let Err(source) = sync_directory(run_directory) {
        // rename 已经让新发现文件可见；父目录同步失败时撤掉本实例内容，不能回报失败却留出伪入口。
        let _ = fs::remove_file(discovery_path);
        let _ = sync_directory(run_directory);
        return Err(EndpointError::Io {
            path: discovery_path.to_owned(),
            source,
        });
    }
    Ok(())
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::*;

    fn runtime_home() -> (tempfile::TempDir, PathBuf) {
        let directory = tempdir().expect("tempdir");
        let home = directory.path().join("runtime-home");
        let run = home.join(RUN_DIRECTORY);
        fs::create_dir_all(&run).expect("run directory");
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).expect("private run mode");
        (directory, home)
    }

    #[tokio::test]
    async fn publish_uses_loopback_private_discovery_and_owned_cleanup() {
        let (_directory, home) = runtime_home();
        let mut endpoint = RuntimeInstanceGuard::acquire(&home)
            .expect("lock")
            .bind_and_publish()
            .await
            .expect("publish");
        assert_eq!(endpoint.address().ip(), Ipv4Addr::LOCALHOST);
        assert_ne!(endpoint.address().port(), 0);
        assert!(endpoint.access_token().len() >= 40);
        assert_eq!(
            fs::metadata(endpoint.discovery_path())
                .expect("discovery metadata")
                .permissions()
                .mode()
                & 0o777,
            PRIVATE_FILE_MODE
        );
        let discovery: RuntimeDiscovery =
            serde_json::from_slice(&fs::read(endpoint.discovery_path()).expect("read discovery"))
                .expect("decode discovery");
        assert_eq!(discovery.address, endpoint.base_url());
        assert_eq!(discovery.access_token, endpoint.access_token());

        drop(endpoint.take_listener());
        let discovery_path = endpoint.discovery_path().to_owned();
        drop(endpoint);
        assert!(!discovery_path.exists());
        assert!(home.join(RUN_DIRECTORY).join(LOCK_FILE).exists());
    }

    #[test]
    fn held_kernel_lock_rejects_a_second_instance() {
        let (_directory, home) = runtime_home();
        let first = RuntimeInstanceGuard::acquire(&home).expect("first lock");
        assert!(matches!(
            RuntimeInstanceGuard::acquire(&home),
            Err(EndpointError::AlreadyRunning { .. })
        ));
        drop(first);
        RuntimeInstanceGuard::acquire(&home).expect("released lock");
    }

    #[tokio::test]
    async fn unsafe_discovery_path_is_preserved() {
        let (_directory, home) = runtime_home();
        let discovery_path = home.join(RUN_DIRECTORY).join(DISCOVERY_FILE);
        fs::create_dir(&discovery_path).expect("conflicting directory");
        let result = RuntimeInstanceGuard::acquire(&home)
            .expect("lock")
            .bind_and_publish()
            .await;
        let Err(error) = result else {
            panic!("unsafe discovery path must be rejected");
        };
        assert!(matches!(error, EndpointError::UnsafeDiscoveryFile { .. }));
        assert!(discovery_path.is_dir());
    }

    #[tokio::test]
    async fn cleanup_preserves_discovery_replaced_by_another_owner() {
        let (_directory, home) = runtime_home();
        let endpoint = RuntimeInstanceGuard::acquire(&home)
            .expect("lock")
            .bind_and_publish()
            .await
            .expect("publish");
        let replacement = RuntimeDiscovery {
            address: endpoint.base_url(),
            instance_id: "replacement-instance".to_owned(),
            access_token: "replacement-token".to_owned(),
            pid: 99,
        };
        fs::write(
            endpoint.discovery_path(),
            serde_json::to_vec(&replacement).expect("encode replacement"),
        )
        .expect("replace discovery");
        let path = endpoint.discovery_path().to_owned();
        drop(endpoint);
        let actual =
            serde_json::from_slice::<RuntimeDiscovery>(&fs::read(path).expect("preserved"))
                .expect("decode");
        assert_eq!(actual.address, replacement.address);
        assert_eq!(actual.instance_id, replacement.instance_id);
        assert_eq!(actual.access_token, replacement.access_token);
        assert_eq!(actual.pid, replacement.pid);
    }
}
