//! 宿主默认规则来源和文件物理路径事实；这里不认识用户身份或 Host 目录布局。

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use agent_tools::{
    AbsolutePath, FileAuthorizationFacts, FileBatchAuthorizationFacts, ResolvedToolInvocation,
};

use super::{PermissionDocument, PermissionStoreFuture};

/// 不可编辑的默认规则来源。每次授权及审批返回后重新加载，与普通规则合并判定。
/// 来源只提供规则，不得根据工具调用直接返回授权结果。
pub trait PermissionRuleSource: Send + Sync {
    fn load(&self) -> PermissionStoreFuture<'_, PermissionDocument>;
}

/// 解析现有路径或最近存在祖先；悬空链接不能按新文件处理。
///
/// # Errors
/// 路径不是绝对路径、链接无法解析或文件系统不可访问时返回错误。
pub fn resolve_permission_path(path: &Path) -> io::Result<PathBuf> {
    let logical = AbsolutePath::new(path.to_owned())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid permission path"))?;
    resolve_ancestor(logical.as_path())
}

fn resolve_ancestor(path: &Path) -> io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // symlink_metadata 存在但 canonicalize 不存在，表示悬空链接或重解析点。
            if std::fs::symlink_metadata(path).is_ok() {
                return Err(error);
            }
            let parent = path.parent().ok_or(error)?;
            let name = path
                .file_name()
                .ok_or_else(|| io::Error::other("invalid path ancestor"))?;
            Ok(resolve_ancestor(parent)?.join(name))
        }
        Err(error) => Err(error),
    }
}

/// 一次授权读取的物理事实；审批等待结束后必须重建。
pub(crate) struct PermissionPaths(BTreeMap<PathBuf, AbsolutePath>);

impl PermissionPaths {
    pub(crate) async fn load(invocation: &ResolvedToolInvocation) -> io::Result<Self> {
        let paths = if let Some(facts) = invocation.facts::<FileAuthorizationFacts>() {
            vec![facts.path.clone()]
        } else if let Some(facts) = invocation.facts::<FileBatchAuthorizationFacts>() {
            facts.paths.clone()
        } else {
            Vec::new()
        };
        if paths.is_empty() {
            return Ok(Self(BTreeMap::new()));
        }
        tokio::task::spawn_blocking(move || {
            paths
                .into_iter()
                .map(|path| {
                    let actual = resolve_permission_path(path.as_path())?;
                    let actual = AbsolutePath::new(actual)
                        .map_err(|_| io::Error::other("invalid physical path"))?;
                    Ok((path.as_path().to_owned(), actual))
                })
                .collect::<io::Result<BTreeMap<_, _>>>()
                .map(Self)
        })
        .await
        .map_err(|_| io::Error::other("permission path task failed"))?
    }

    pub(crate) fn actual<'a>(&'a self, path: &'a AbsolutePath) -> &'a AbsolutePath {
        self.0.get(path.as_path()).unwrap_or(path)
    }
}
