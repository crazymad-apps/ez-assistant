//! 受管文件的临时目录与发布路径。SQLite 清单提交前，目录只属于当前执行的归档重试。

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

use assistant_protocol::{
    ScheduleExecutionId, ScheduleResultFile, SessionArtifact, SessionResourceRoot,
};
use assistant_runtime::SessionExecutionEnvironment;

use crate::{
    http::resources::files::{canonicalize, open_resolved},
    storage::{
        StorageResult,
        filesystem::{create_new_private_file, sync_directory},
        internal_error, invalid_data,
    },
};

pub(super) fn archive(
    root: &Path,
    execution: &ScheduleExecutionId,
    environment: &SessionExecutionEnvironment,
    artifacts: &[SessionArtifact],
    paths: &crate::user_paths::UserPaths,
) -> StorageResult<Vec<ScheduleResultFile>> {
    let name = component(execution.as_str())?;
    crate::config_source::prepare_private_directory(root)
        .map_err(|error| internal_error("schedule result root could not be prepared", error))?;
    let staging_name = format!("staging-{name}");
    // files_json 为空时，既有发布目录来自上一次未确认写入，可安全重建。
    remove_owned_directory(root, &staging_name)?;
    remove_owned_directory(root, name)?;
    let staging = root.join(&staging_name);
    let assets = staging.join("artifacts");
    fs::create_dir(&staging).map_err(io_error)?;
    fs::create_dir(&assets).map_err(io_error)?;
    let files = artifacts
        .iter()
        .enumerate()
        .map(|(index, artifact)| {
            let (path, name) = artifact_path(environment, artifact)?;
            // 使用与声明时相同的用户域权限，包括 Host 运行期更新的保护路径。
            let allowed = paths.resolve(&path, false).map_err(io_error)?;
            if allowed != path {
                return Err(invalid_data(
                    "scheduled artifact path changed during validation",
                ));
            }
            let mut source = open_resolved(&path, false)
                .map_err(|_| invalid_data("scheduled artifact is not a readable regular file"))?;
            let before = source.metadata().map_err(io_error)?;
            let file_id = index.to_string();
            let mut target = create_new_private_file(&assets.join(&file_id))?;
            let size_bytes = io::copy(&mut source, &mut target).map_err(io_error)?;
            let after = source.metadata().map_err(io_error)?;
            if size_bytes != after.len()
                || before.len() != after.len()
                || before.modified().ok() != after.modified().ok()
            {
                return Err(invalid_data(
                    "scheduled artifact changed while being copied",
                ));
            }
            target.sync_all().map_err(io_error)?;
            Ok(ScheduleResultFile {
                file_id,
                name,
                title: artifact.title.clone(),
                size_bytes,
            })
        })
        .collect::<StorageResult<Vec<_>>>()?;
    sync_directory(&assets)?;
    sync_directory(&staging)?;
    fs::rename(staging, root.join(name)).map_err(io_error)?;
    sync_directory(root)?;
    Ok(files)
}

fn artifact_path(
    environment: &SessionExecutionEnvironment,
    artifact: &SessionArtifact,
) -> StorageResult<(PathBuf, String)> {
    let claimed = match artifact.locator.root {
        SessionResourceRoot::SessionPrivate => &environment.session_private_directory,
        SessionResourceRoot::WorkspacePrimary if environment.workspace_id.is_some() => {
            &environment.working_directory
        }
        SessionResourceRoot::WorkspaceAdditional { directory_index } => environment
            .additional_workspace_directories
            .get(directory_index as usize)
            .ok_or_else(|| invalid_data("artifact directory index is invalid"))?,
        _ => return Err(invalid_data("artifact workspace root is unavailable")),
    };
    let relative = Path::new(&artifact.locator.relative_path);
    if !relative
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(invalid_data("scheduled artifact path is invalid"));
    }
    let name = relative
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid_data("scheduled artifact filename is invalid"))?;
    let root_metadata = fs::symlink_metadata(claimed).map_err(io_error)?;
    if !root_metadata.is_dir() || crate::platform::is_reparse_or_link(&root_metadata) {
        return Err(invalid_data(
            "scheduled artifact root is no longer a directory",
        ));
    }
    let root = canonicalize(Path::new(claimed))
        .map_err(|_| invalid_data("scheduled artifact root is unavailable"))?;
    let path = canonicalize(&root.join(relative))
        .map_err(|_| invalid_data("scheduled artifact is unavailable"))?;
    if !path.starts_with(&root) {
        return Err(invalid_data(
            "scheduled artifact is outside its frozen root",
        ));
    }
    Ok((path, name.to_owned()))
}

pub(super) fn result_path(
    root: &Path,
    execution: &ScheduleExecutionId,
    file: &ScheduleResultFile,
) -> StorageResult<PathBuf> {
    let root =
        canonicalize(root).map_err(|_| invalid_data("schedule result root is unavailable"))?;
    Ok(root
        .join(component(execution.as_str())?)
        .join("artifacts")
        .join(component(&file.file_id)?))
}

pub(super) fn remove_owned_directory(root: &Path, name: &str) -> StorageResult<()> {
    let path = root.join(component(name)?);
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() && !crate::platform::is_reparse_or_link(&metadata) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
        Ok(_) => return Err(invalid_data("schedule directory root changed")),
    }
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() && !crate::platform::is_reparse_or_link(&metadata) => {
            fs::remove_dir_all(&path).map_err(io_error)?;
            sync_directory(root)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
        Ok(_) => Err(invalid_data("schedule directory changed")),
    }
}

fn component(name: &str) -> StorageResult<&str> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(invalid_data("schedule file identity is invalid"));
    }
    Ok(name)
}

fn io_error(error: io::Error) -> assistant_runtime::StoreError {
    internal_error("schedule result directory could not be updated", error)
}
