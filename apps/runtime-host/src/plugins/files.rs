//! 用户域包文件端口：归档准备、无覆盖发布、按操作收据收尾。

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};

use assistant_protocol::{
    IdempotencyKey, PluginArchiveFormat, PluginPackageOrigin, PluginPackageSelection,
};
use assistant_runtime::{
    CommittedPluginChange, PluginAudit, PluginFileError, PluginFilePort, PluginMcpDiscovery,
    PreparedPluginPackage, StoredPlugin, StoredPluginOperation,
};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::{
    PackageError,
    archive::{ArchiveFormat, prepare_archive_with_cancel},
    paths::PluginPaths,
};

#[derive(Clone)]
pub(crate) struct HostPluginFiles {
    user_root: PathBuf,
    center: Option<(
        Arc<crate::access::Credentials>,
        crate::access::enterprise::UserKey,
    )>,
}

impl HostPluginFiles {
    pub(crate) fn new(user_root: PathBuf) -> Self {
        Self {
            user_root,
            center: None,
        }
    }

    pub(crate) fn with_center(
        mut self,
        credentials: Arc<crate::access::Credentials>,
        key: crate::access::enterprise::UserKey,
    ) -> Self {
        self.center = Some((credentials, key));
        self
    }

    pub(crate) fn upload_path(&self, key: &IdempotencyKey) -> Result<PathBuf, PluginFileError> {
        let paths = PluginPaths::prepare(&self.user_root).map_err(unavailable)?;
        let operation = paths.operation(key, false);
        crate::config_source::prepare_private_directory(&operation)
            .map_err(|_| PluginFileError::Unavailable)?;
        Ok(operation.join("archive"))
    }

    fn prepare_blocking(
        &self,
        key: &IdempotencyKey,
        selection: &PluginPackageSelection,
        token: &CancellationToken,
    ) -> Result<PreparedPluginPackage, PluginFileError> {
        if token.is_cancelled() {
            return Err(PluginFileError::Cancelled);
        }
        let paths = PluginPaths::prepare(&self.user_root).map_err(unavailable)?;
        let operation = paths.operation(key, false);
        crate::config_source::prepare_private_directory(&operation)
            .map_err(|_| PluginFileError::Unavailable)?;
        let archive = operation.join("archive");
        let metadata = fs::symlink_metadata(&archive).map_err(unavailable)?;
        if !metadata.file_type().is_file() || metadata.len() != selection.size_bytes {
            return Err(PluginFileError::Changed);
        }
        let format = match selection.format {
            PluginArchiveFormat::Zip => ArchiveFormat::Zip,
            PluginArchiveFormat::TarGzip => ArchiveFormat::TarGzip,
        };
        let prepared = prepare_archive_with_cancel(&archive, format, &operation, token)
            .map_err(package_error)?;
        if token.is_cancelled() {
            prepared.staging.close().map_err(unavailable)?;
            return Err(PluginFileError::Cancelled);
        }
        if prepared.sha256 != selection.sha256
            || prepared.manifest.name != selection.name
            || prepared.manifest.version != selection.version
        {
            prepared.staging.close().map_err(unavailable)?;
            return Err(PluginFileError::Changed);
        }
        let manifest_json =
            fs::read_to_string(prepared.staging.path().join("plugin.json")).map_err(unavailable)?;
        let extracted = operation.join("extracted");
        if fs::symlink_metadata(&extracted).is_ok() {
            return Err(PluginFileError::CleanupRequired);
        }
        fs::rename(prepared.staging.path(), &extracted).map_err(unavailable)?;
        let _ = prepared.staging.keep();
        sync_directory(&operation)?;
        Ok(PreparedPluginPackage {
            manifest_json,
            sha256: prepared.sha256,
        })
    }

    fn publish_blocking(
        &self,
        key: &IdempotencyKey,
        selection: &PluginPackageSelection,
    ) -> Result<(), PluginFileError> {
        let paths = PluginPaths::prepare(&self.user_root).map_err(unavailable)?;
        let plugin_root = paths.root().join(&selection.name);
        let new_root = match fs::symlink_metadata(&plugin_root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => return Err(unavailable(error)),
            Ok(_) => false,
        };
        if new_root {
            crate::config_source::prepare_private_directory(&plugin_root)
                .map_err(|_| PluginFileError::Unavailable)?;
            let marker = plugin_root.join(".host-operation-owner");
            fs::write(&marker, operation_owner(key)).map_err(unavailable)?;
            crate::platform::sync_file_at(&marker).map_err(unavailable)?;
            sync_directory(&plugin_root)?;
        }
        paths.ensure_plugin(&selection.name).map_err(unavailable)?;
        let from = paths.operation(key, false).join("extracted");
        let target = paths
            .package(&selection.name, &selection.sha256)
            .map_err(unavailable)?;
        if fs::symlink_metadata(&target).is_ok() {
            return Err(PluginFileError::CleanupRequired);
        }
        let marker = from.join(".host-operation-owner");
        fs::write(&marker, operation_owner(key)).map_err(unavailable)?;
        crate::platform::sync_file_at(&marker).map_err(unavailable)?;
        sync_directory(&from)?;
        fs::rename(from, &target).map_err(unavailable)?;
        sync_directory(target.parent().ok_or(PluginFileError::Unavailable)?)?;
        Ok(())
    }

    fn discard_blocking(
        &self,
        key: &IdempotencyKey,
        selection: Option<&PluginPackageSelection>,
    ) -> Result<(), PluginFileError> {
        let paths = PluginPaths::prepare(&self.user_root).map_err(unavailable)?;
        if let Some(selection) = selection
            && selection.expected_active_sha256.as_deref() != Some(&selection.sha256)
        {
            let target = paths
                .package(&selection.name, &selection.sha256)
                .map_err(unavailable)?;
            if fs::read_to_string(target.join(".host-operation-owner"))
                .ok()
                .as_deref()
                == Some(operation_owner(key).as_str())
            {
                remove_owned_directory(&target)?;
            }
        }
        remove_owned_directory(&paths.operation(key, false))?;
        if let Some(selection) = selection {
            remove_new_plugin_root(&paths, key, &selection.name)?;
        }
        Ok(())
    }

    fn finish_blocking(
        &self,
        key: &IdempotencyKey,
        change: &CommittedPluginChange,
    ) -> Result<(), PluginFileError> {
        let paths = PluginPaths::prepare(&self.user_root).map_err(unavailable)?;
        let trash = paths.operation(key, true);
        crate::config_source::prepare_private_directory(&trash)
            .map_err(|_| PluginFileError::CleanupRequired)?;
        if let Some(previous) = &change.previous_sha256
            && change.next_sha256.as_deref() != Some(previous)
        {
            let source = paths.package(&change.name, previous).map_err(unavailable)?;
            move_to_trash_and_remove(&source, &trash.join("previous-package"))?;
        }
        if change.clear_data {
            move_to_trash_and_remove(
                &paths.config(&change.name).map_err(unavailable)?,
                &trash.join("config"),
            )?;
            move_to_trash_and_remove(
                &paths.data(&change.name).map_err(unavailable)?,
                &trash.join("data"),
            )?;
        }
        remove_owned_directory(&paths.operation(key, false))?;
        remove_owned_directory(&trash)?;
        let marker = paths
            .root()
            .join(&change.name)
            .join(".host-operation-owner");
        if fs::read_to_string(&marker).ok().as_deref() == Some(operation_owner(key).as_str()) {
            fs::remove_file(marker).map_err(|_| PluginFileError::CleanupRequired)?;
        }
        if change.clear_data {
            remove_empty_directory(&paths.root().join(&change.name).join("packages"))?;
            remove_empty_directory(&paths.root().join(&change.name))?;
        }
        Ok(())
    }

    fn audit_blocking(
        &self,
        plugins: &[StoredPlugin],
        operations: &[StoredPluginOperation],
    ) -> Result<PluginAudit, PluginFileError> {
        let paths = PluginPaths::prepare(&self.user_root).map_err(unavailable)?;
        let mut audit = PluginAudit::default();
        for plugin in plugins {
            if let Some(sha) = &plugin.active_sha256 {
                let package = paths.package(&plugin.name, sha).map_err(unavailable)?;
                if !fs::symlink_metadata(&package)
                    .is_ok_and(|metadata| metadata.file_type().is_dir())
                    || fs::read_to_string(package.join("plugin.json"))
                        .ok()
                        .as_deref()
                        != plugin.active_manifest_json.as_deref()
                {
                    audit.corrupt_names.insert(plugin.name.clone());
                }
            }
        }
        for temporary in [false, true] {
            let root = paths.operation(
                &IdempotencyKey::new("audit-placeholder")
                    .map_err(|_| PluginFileError::Unavailable)?,
                temporary,
            );
            let parent = root.parent().ok_or(PluginFileError::Unavailable)?;
            for entry in fs::read_dir(parent).map_err(unavailable)? {
                let entry = entry.map_err(unavailable)?;
                let file_type = entry.file_type().map_err(unavailable)?;
                if !file_type.is_dir()
                    || !operations.iter().any(|op| {
                        paths.operation(&op.key, temporary) == entry.path()
                            && (matches!(
                                op.phase,
                                assistant_runtime::PluginOperationPhase::Accepted
                                    | assistant_runtime::PluginOperationPhase::Preparing
                                    | assistant_runtime::PluginOperationPhase::Committing
                                    | assistant_runtime::PluginOperationPhase::Cleaning
                            ) || op.error_code.as_deref() == Some("cleanup_required"))
                    })
                {
                    audit.cleanup_blocked = true;
                }
            }
        }
        for entry in fs::read_dir(paths.root()).map_err(unavailable)? {
            let entry = entry.map_err(unavailable)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                audit.cleanup_blocked = true;
                continue;
            };
            if matches!(name, ".staging" | ".trash") {
                continue;
            }
            if !super::plugin_name(name) || !entry.file_type().map_err(unavailable)?.is_dir() {
                audit.cleanup_blocked = true;
                continue;
            }
            if !plugins.iter().any(|plugin| plugin.name == name)
                && !operations
                    .iter()
                    .any(|operation| operation_may_own_root(operation, name))
            {
                audit.cleanup_blocked = true;
                continue;
            }
            let packages = entry.path().join("packages");
            match fs::symlink_metadata(&packages) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(unavailable(error)),
                Ok(metadata) if metadata.file_type().is_dir() => {}
                Ok(_) => {
                    audit.cleanup_blocked = true;
                    continue;
                }
            }
            for package in fs::read_dir(packages).map_err(unavailable)? {
                let package = package.map_err(unavailable)?;
                let sha = package.file_name();
                let Some(sha) = sha.to_str() else {
                    audit.cleanup_blocked = true;
                    continue;
                };
                if !package.file_type().map_err(unavailable)?.is_dir()
                    || !plugins.iter().any(|plugin| {
                        plugin.name == name && plugin.active_sha256.as_deref() == Some(sha)
                    }) && !operations
                        .iter()
                        .any(|op| operation_may_own_package(op, name, sha))
                {
                    audit.cleanup_blocked = true;
                }
            }
        }
        Ok(audit)
    }

    async fn download(
        &self,
        key: &IdempotencyKey,
        selection: &PluginPackageSelection,
        cancellation: &CancellationToken,
        progress: &Arc<dyn Fn(u64) + Send + Sync>,
    ) -> Result<(), PluginFileError> {
        let (credentials, user) = self.center.as_ref().ok_or(PluginFileError::Unavailable)?;
        let PluginPackageOrigin::Center { center_id } = &selection.origin else {
            return Err(PluginFileError::Invalid);
        };
        if &user.center_id != center_id {
            return Err(PluginFileError::Invalid);
        }
        let (client, token) = credentials
            .current_plugin_center(user)
            .map_err(|_| PluginFileError::Unavailable)?;
        let response = client
            .download_plugin(&token, selection)
            .await
            .map_err(center_error)?;
        let path = self.upload_path(key)?;
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path).map_err(unavailable)?;
        let mut file = tokio::fs::File::from_std(file);
        let mut stream = response.bytes_stream();
        let started = tokio::time::Instant::now();
        let mut size = 0_u64;
        let mut hash = Sha256::new();
        loop {
            if cancellation.is_cancelled() || started.elapsed() > Duration::from_secs(30 * 60) {
                return Err(PluginFileError::Cancelled);
            }
            let chunk = match tokio::time::timeout(Duration::from_secs(60), stream.next()).await {
                Ok(Some(Ok(chunk))) => chunk,
                Ok(None) => break,
                Ok(Some(Err(_))) | Err(_) => return Err(PluginFileError::Unavailable),
            };
            size = size
                .checked_add(chunk.len() as u64)
                .filter(|size| *size <= selection.size_bytes)
                .ok_or(PluginFileError::Changed)?;
            progress(size);
            hash.update(&chunk);
            file.write_all(&chunk).await.map_err(unavailable)?;
        }
        if size != selection.size_bytes || format!("{:x}", hash.finalize()) != selection.sha256 {
            return Err(PluginFileError::Changed);
        }
        file.flush().await.map_err(unavailable)?;
        file.sync_all().await.map_err(unavailable)?;
        Ok(())
    }
}

impl PluginFilePort for HostPluginFiles {
    fn mcp_candidates(
        &self,
        plugins: Vec<StoredPlugin>,
    ) -> BoxFuture<'_, Result<PluginMcpDiscovery, PluginFileError>> {
        let user_root = self.user_root.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                super::capabilities::discover_mcp(&user_root, plugins)
            })
            .await
            .map_err(|_| PluginFileError::Unavailable)
        })
    }
    fn prepare(
        &self,
        key: IdempotencyKey,
        selection: PluginPackageSelection,
        cancellation: CancellationToken,
        progress: Arc<dyn Fn(u64) + Send + Sync>,
    ) -> BoxFuture<'_, Result<PreparedPluginPackage, PluginFileError>> {
        Box::pin(async move {
            static BLOCKING: OnceLock<Arc<Semaphore>> = OnceLock::new();
            let slots = BLOCKING.get_or_init(|| Arc::new(Semaphore::new(2))).clone();
            let permit = slots
                .try_acquire_owned()
                .map_err(|_| PluginFileError::Busy)?;
            if matches!(selection.origin, PluginPackageOrigin::Center { .. }) {
                self.download(&key, &selection, &cancellation, &progress)
                    .await?;
            }
            let this = self.clone();
            let scan_cancel = cancellation.child_token();
            let worker_cancel = scan_cancel.clone();
            let mut scan = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                this.prepare_blocking(&key, &selection, &worker_cancel)
            });
            match tokio::time::timeout(Duration::from_secs(5 * 60), &mut scan).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err(PluginFileError::Unavailable),
                Err(_) => {
                    scan_cancel.cancel();
                    let _ = scan.await;
                    Err(PluginFileError::Unavailable)
                }
            }
        })
    }
    fn publish(
        &self,
        key: IdempotencyKey,
        selection: PluginPackageSelection,
    ) -> BoxFuture<'_, Result<(), PluginFileError>> {
        let this = self.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || this.publish_blocking(&key, &selection))
                .await
                .map_err(|_| PluginFileError::Unavailable)?
        })
    }
    fn discard(
        &self,
        key: IdempotencyKey,
        selection: Option<PluginPackageSelection>,
    ) -> BoxFuture<'_, Result<(), PluginFileError>> {
        let this = self.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || this.discard_blocking(&key, selection.as_ref()))
                .await
                .map_err(|_| PluginFileError::Unavailable)?
        })
    }
    fn finish(
        &self,
        key: IdempotencyKey,
        change: CommittedPluginChange,
    ) -> BoxFuture<'_, Result<(), PluginFileError>> {
        let this = self.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || this.finish_blocking(&key, &change))
                .await
                .map_err(|_| PluginFileError::CleanupRequired)?
        })
    }
    fn audit(
        &self,
        plugins: Vec<StoredPlugin>,
        operations: Vec<StoredPluginOperation>,
    ) -> BoxFuture<'_, Result<PluginAudit, PluginFileError>> {
        let this = self.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || this.audit_blocking(&plugins, &operations))
                .await
                .map_err(|_| PluginFileError::Unavailable)?
        })
    }
}

fn move_to_trash_and_remove(source: &Path, trash: &Path) -> Result<(), PluginFileError> {
    if fs::symlink_metadata(source).is_ok() {
        if fs::symlink_metadata(trash).is_ok() {
            return Err(PluginFileError::CleanupRequired);
        }
        let metadata = fs::symlink_metadata(source).map_err(unavailable)?;
        if !metadata.file_type().is_dir() {
            return Err(PluginFileError::CleanupRequired);
        }
        fs::rename(source, trash).map_err(|_| PluginFileError::CleanupRequired)?;
        sync_directory(trash.parent().ok_or(PluginFileError::CleanupRequired)?)?;
    }
    remove_owned_directory(trash)
}

fn remove_owned_directory(path: &Path) -> Result<(), PluginFileError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            fs::remove_dir_all(path).map_err(|_| PluginFileError::CleanupRequired)
        }
        Ok(_) => Err(PluginFileError::CleanupRequired),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(PluginFileError::CleanupRequired),
    }
}

fn remove_empty_directory(path: &Path) -> Result<(), PluginFileError> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(PluginFileError::CleanupRequired),
    }
}

fn operation_owner(key: &IdempotencyKey) -> String {
    format!("{:x}", Sha256::digest(key.as_str().as_bytes()))
}

fn remove_new_plugin_root(
    paths: &PluginPaths,
    key: &IdempotencyKey,
    name: &str,
) -> Result<(), PluginFileError> {
    let root = paths.root().join(name);
    let marker = root.join(".host-operation-owner");
    if fs::read_to_string(&marker).ok().as_deref() != Some(operation_owner(key).as_str()) {
        return Ok(());
    }
    for directory in ["config", "data", "packages"] {
        remove_empty_directory(&root.join(directory))?;
    }
    fs::remove_file(marker).map_err(|_| PluginFileError::CleanupRequired)?;
    remove_empty_directory(&root)
}

fn sync_directory(path: &Path) -> Result<(), PluginFileError> {
    crate::platform::sync_directory(path).map_err(unavailable)
}

fn package_error(error: PackageError) -> PluginFileError {
    match error {
        PackageError::Invalid | PackageError::TooLarge => PluginFileError::Invalid,
        PackageError::Cancelled => PluginFileError::Cancelled,
        PackageError::Io(_) => PluginFileError::Unavailable,
    }
}
fn unavailable(_: io::Error) -> PluginFileError {
    PluginFileError::Unavailable
}
fn operation_may_own_package(operation: &StoredPluginOperation, name: &str, sha: &str) -> bool {
    if !matches!(
        operation.phase,
        assistant_runtime::PluginOperationPhase::Accepted
            | assistant_runtime::PluginOperationPhase::Preparing
            | assistant_runtime::PluginOperationPhase::Committing
            | assistant_runtime::PluginOperationPhase::Cleaning
    ) && operation.error_code.as_deref() != Some("cleanup_required")
    {
        return false;
    }
    let selected = serde_json::from_str::<assistant_protocol::BeginPluginInstallRequest>(
        &operation.intent_json,
    )
    .ok()
    .is_some_and(|intent| {
        intent
            .packages
            .iter()
            .any(|item| item.name == name && item.sha256 == sha)
    });
    selected
        || serde_json::from_str::<Vec<CommittedPluginChange>>(&operation.committed_json)
            .ok()
            .is_some_and(|changes| {
                changes.iter().any(|change| {
                    change.name == name && change.previous_sha256.as_deref() == Some(sha)
                })
            })
}
fn operation_may_own_root(operation: &StoredPluginOperation, name: &str) -> bool {
    if !matches!(
        operation.phase,
        assistant_runtime::PluginOperationPhase::Accepted
            | assistant_runtime::PluginOperationPhase::Preparing
            | assistant_runtime::PluginOperationPhase::Committing
            | assistant_runtime::PluginOperationPhase::Cleaning
    ) && operation.error_code.as_deref() != Some("cleanup_required")
    {
        return false;
    }
    serde_json::from_str::<assistant_protocol::BeginPluginInstallRequest>(&operation.intent_json)
        .ok()
        .is_some_and(|intent| intent.packages.iter().any(|item| item.name == name))
}
fn center_error(
    error: crate::access::enterprise::client::plugins::PluginCenterError,
) -> PluginFileError {
    use crate::access::enterprise::client::plugins::PluginCenterError;
    match error {
        PluginCenterError::Changed => PluginFileError::Changed,
        PluginCenterError::Invalid => PluginFileError::Invalid,
        PluginCenterError::NotListed | PluginCenterError::Unavailable => {
            PluginFileError::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_prepared_package_with_platform_file_sync() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(".runtime-test");
        fs::create_dir_all(&base).unwrap();
        let home = tempfile::Builder::new()
            .prefix("plugin-publish-")
            .tempdir_in(base)
            .unwrap();
        let files = HostPluginFiles::new(home.path().to_path_buf());
        let key = IdempotencyKey::new("publish-platform-sync").unwrap();
        let selection = PluginPackageSelection {
            name: "example-plugin".to_owned(),
            version: "1.0.0".to_owned(),
            sha256: "a".repeat(64),
            size_bytes: 1,
            format: PluginArchiveFormat::TarGzip,
            origin: PluginPackageOrigin::Upload,
            expected_active_sha256: None,
        };
        let paths = PluginPaths::prepare(home.path()).unwrap();
        fs::create_dir_all(paths.operation(&key, false).join("extracted")).unwrap();

        files.publish_blocking(&key, &selection).unwrap();

        assert!(
            paths
                .package(&selection.name, &selection.sha256)
                .unwrap()
                .is_dir()
        );
    }

    #[test]
    fn audit_isolates_corrupt_package_and_blocks_unowned_process_content() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        fs::create_dir_all(&base).unwrap();
        let isolated = tempfile::Builder::new()
            .prefix("c03-m1-audit-")
            .tempdir_in(base)
            .unwrap();
        let files = HostPluginFiles::new(isolated.path().to_path_buf());
        let paths = PluginPaths::prepare(isolated.path()).unwrap();
        let name = "document-tools";
        let sha = "a".repeat(64);
        paths.ensure_plugin(name).unwrap();
        let package = paths.package(name, &sha).unwrap();
        fs::create_dir(&package).unwrap();
        let manifest = r#"{"name":"document-tools"}"#;
        fs::write(package.join("plugin.json"), manifest).unwrap();
        let plugin = StoredPlugin {
            name: name.into(),
            source: assistant_runtime::PluginSource::Upload,
            active_version: Some("1.0.0".into()),
            active_sha256: Some(sha),
            active_manifest_json: Some(manifest.into()),
            enabled: true,
            parameters_json: "{}".into(),
            software_paths_json: "{}".into(),
            market_status: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        };
        assert_eq!(
            files
                .audit_blocking(std::slice::from_ref(&plugin), &[])
                .unwrap(),
            PluginAudit::default()
        );
        fs::remove_file(package.join("plugin.json")).unwrap();
        let audit = files
            .audit_blocking(std::slice::from_ref(&plugin), &[])
            .unwrap();
        assert_eq!(audit.corrupt_names, [name.to_owned()].into());
        assert!(!audit.cleanup_blocked);
        fs::create_dir(paths.root().join("orphan-tools")).unwrap();
        assert!(
            files
                .audit_blocking(std::slice::from_ref(&plugin), &[])
                .unwrap()
                .cleanup_blocked
        );
        fs::create_dir(paths.root().join(".staging/unowned")).unwrap();
        let audit = files.audit_blocking(&[plugin], &[]).unwrap();
        assert!(audit.cleanup_blocked);
        assert_eq!(audit.corrupt_names, [name.to_owned()].into());
    }

    #[test]
    fn explicit_clear_removes_only_owned_plugin_content() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        fs::create_dir_all(&base).unwrap();
        let isolated = tempfile::Builder::new()
            .prefix("c03-m1-clear-")
            .tempdir_in(base)
            .unwrap();
        let files = HostPluginFiles::new(isolated.path().to_path_buf());
        let paths = PluginPaths::prepare(isolated.path()).unwrap();
        let name = "document-tools";
        let other = "other-tools";
        let sha = "a".repeat(64);
        for plugin in [name, other] {
            paths.ensure_plugin(plugin).unwrap();
            fs::write(paths.config(plugin).unwrap().join("settings.conf"), plugin).unwrap();
            fs::write(paths.data(plugin).unwrap().join("result.txt"), plugin).unwrap();
        }
        let package = paths.package(name, &sha).unwrap();
        fs::create_dir(&package).unwrap();
        fs::write(package.join("plugin.json"), "{}").unwrap();
        let key = IdempotencyKey::new("explicit-clear").unwrap();
        files
            .finish_blocking(
                &key,
                &CommittedPluginChange {
                    name: name.into(),
                    previous_sha256: Some(sha),
                    next_sha256: None,
                    clear_data: true,
                },
            )
            .unwrap();
        assert!(!package.exists());
        assert!(!paths.config(name).unwrap().exists());
        assert!(!paths.data(name).unwrap().exists());
        assert!(!paths.root().join(name).exists());
        assert!(paths.config(other).unwrap().join("settings.conf").is_file());
        assert!(paths.data(other).unwrap().join("result.txt").is_file());
        assert_eq!(
            fs::read_dir(paths.root().join(".trash")).unwrap().count(),
            0
        );
    }

    #[test]
    fn failed_new_publication_discards_new_plugin_root() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        fs::create_dir_all(&base).unwrap();
        let isolated = tempfile::Builder::new()
            .prefix("c03-m1-discard-")
            .tempdir_in(base)
            .unwrap();
        let files = HostPluginFiles::new(isolated.path().to_path_buf());
        let paths = PluginPaths::prepare(isolated.path()).unwrap();
        let key = IdempotencyKey::new("publish-then-discard").unwrap();
        let selection = PluginPackageSelection {
            name: "document-tools".into(),
            version: "1.0.0".into(),
            sha256: "a".repeat(64),
            size_bytes: 1,
            format: PluginArchiveFormat::TarGzip,
            origin: PluginPackageOrigin::Upload,
            expected_active_sha256: None,
        };
        let operation = paths.operation(&key, false);
        fs::create_dir(&operation).unwrap();
        let extracted = operation.join("extracted");
        fs::create_dir(&extracted).unwrap();
        fs::write(extracted.join("plugin.json"), "{}").unwrap();
        files.publish_blocking(&key, &selection).unwrap();
        assert!(
            paths
                .package(&selection.name, &selection.sha256)
                .unwrap()
                .is_dir()
        );
        files.discard_blocking(&key, Some(&selection)).unwrap();
        assert!(!paths.root().join(&selection.name).exists());
        assert_eq!(
            fs::read_dir(paths.root().join(".staging")).unwrap().count(),
            0
        );
    }

    #[test]
    #[ignore = "set C03_SAMPLE_ARCHIVE to the preserved valid TAR.GZ fixture"]
    fn preserved_package_publishes_and_uninstalls_only_in_new_isolated_home() {
        let source = PathBuf::from(std::env::var_os("C03_SAMPLE_ARCHIVE").expect("sample archive"));
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        fs::create_dir_all(&root).unwrap();
        let isolated = tempfile::Builder::new()
            .prefix("c03-m1-real-package-")
            .tempdir_in(root)
            .unwrap();
        let files = HostPluginFiles::new(isolated.path().to_path_buf());
        let key = IdempotencyKey::new("real-cad-package-install").unwrap();
        let selection = PluginPackageSelection {
            name: "warehouse-cad-guidance".into(),
            version: "0.1.0".into(),
            sha256: "8e65947a8315c382db4f43ad36d6bb4423a292a8886ec7e5ba7b88c34bbacc52".into(),
            size_bytes: 7_852_108,
            format: PluginArchiveFormat::TarGzip,
            origin: PluginPackageOrigin::Upload,
            expected_active_sha256: None,
        };
        let source_hash = Sha256::digest(fs::read(&source).unwrap());
        fs::copy(&source, files.upload_path(&key).unwrap()).unwrap();
        let prepared = files
            .prepare_blocking(&key, &selection, &CancellationToken::new())
            .unwrap();
        assert_eq!(prepared.sha256, selection.sha256);
        files.publish_blocking(&key, &selection).unwrap();
        let paths = PluginPaths::prepare(isolated.path()).unwrap();
        let package = paths.package(&selection.name, &selection.sha256).unwrap();
        assert!(
            package
                .join("skills/compact-shelving-cad/SKILL.md")
                .is_file()
        );
        files
            .finish_blocking(
                &key,
                &CommittedPluginChange {
                    name: selection.name.clone(),
                    previous_sha256: None,
                    next_sha256: Some(selection.sha256.clone()),
                    clear_data: false,
                },
            )
            .unwrap();
        assert_eq!(
            fs::read_dir(paths.root().join(".staging")).unwrap().count(),
            0
        );
        assert_eq!(
            fs::read_dir(paths.root().join(".trash")).unwrap().count(),
            0
        );
        let uninstall = IdempotencyKey::new("real-cad-package-uninstall").unwrap();
        files
            .finish_blocking(
                &uninstall,
                &CommittedPluginChange {
                    name: selection.name.clone(),
                    previous_sha256: Some(selection.sha256),
                    next_sha256: None,
                    clear_data: false,
                },
            )
            .unwrap();
        assert!(!package.exists());
        assert!(paths.config(&selection.name).unwrap().is_dir());
        assert!(paths.data(&selection.name).unwrap().is_dir());
        assert_eq!(
            fs::read_dir(paths.root().join(".staging")).unwrap().count(),
            0
        );
        assert_eq!(
            fs::read_dir(paths.root().join(".trash")).unwrap().count(),
            0
        );
        assert_eq!(Sha256::digest(fs::read(&source).unwrap()), source_hash);
    }
}
