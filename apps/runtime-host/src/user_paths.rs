//! 按用户布局装配默认文件权限规则；工具与 HTTP 共用权限匹配器。

use std::sync::{Arc, RwLock};
use std::{
    io,
    path::{Path, PathBuf},
};

/// 装配时冻结的文件归属。既有权限只能进一步收紧，不能放行这些受保护路径。
#[derive(Clone)]
pub(crate) struct UserPaths {
    pub(crate) user_root: PathBuf,
    host_root: PathBuf,
    protected_files: Arc<RwLock<Vec<PathBuf>>>,
}

impl UserPaths {
    pub(crate) fn new(user_root: &Path, protected_files: Vec<PathBuf>) -> Self {
        Self::with_secrets(user_root, Arc::new(RwLock::new(protected_files)))
    }

    pub(crate) fn with_secrets(
        user_root: &Path,
        protected_files: Arc<RwLock<Vec<PathBuf>>>,
    ) -> Self {
        // 正式布局在监听前完成准入；独立测试 Store 仍可显式使用自己的临时根。
        let host_root = user_root
            .parent()
            .filter(|p| p.file_name().is_some_and(|n| n == "users"))
            .and_then(Path::parent)
            .unwrap_or(user_root)
            .to_owned();
        Self {
            user_root: user_root.to_owned(),
            host_root,
            protected_files,
        }
    }

    /// 提供普通文件 Deny 规则；布局仅决定规则数据，授权决定统一由权限匹配器完成。
    pub(crate) fn permission_document(&self) -> io::Result<assistant_runtime::PermissionDocument> {
        use assistant_runtime::{PathMatch, PermissionDocument, PermissionFileOperation as Op};
        let mut document = PermissionDocument::empty();
        let secrets = self.protected_files.read().map_err(|_| denied())?.clone();
        let host = assistant_runtime::resolve_permission_path(&self.host_root)?;
        let user = assistant_runtime::resolve_permission_path(&self.user_root)?;
        for (host, user) in [(&self.host_root, &self.user_root), (&host, &user)] {
            for operation in [
                Op::Read,
                Op::List,
                Op::Find,
                Op::Search,
                Op::Write,
                Op::Edit,
                Op::Delete,
            ] {
                let mutation = matches!(operation, Op::Write | Op::Edit | Op::Delete);
                let mut exclusions = vec![user.clone()];
                if !mutation {
                    exclusions.push(host.join("skills"));
                }
                push_rule(
                    &mut document,
                    operation,
                    host,
                    PathMatch::Recursive,
                    exclusions,
                )?;
                if mutation {
                    for ancestor in user.ancestors().skip(1) {
                        push_rule(
                            &mut document,
                            operation,
                            ancestor,
                            PathMatch::Exact,
                            Vec::new(),
                        )?;
                    }
                }
            }
        }
        for secret in secrets {
            for path in [
                &secret,
                &assistant_runtime::resolve_permission_path(&secret)?,
            ] {
                for operation in [
                    Op::Read,
                    Op::List,
                    Op::Find,
                    Op::Search,
                    Op::Write,
                    Op::Edit,
                    Op::Delete,
                ] {
                    push_rule(&mut document, operation, path, PathMatch::Exact, Vec::new())?;
                    if matches!(operation, Op::Write | Op::Edit | Op::Delete) {
                        for ancestor in path.ancestors().skip(1) {
                            push_rule(
                                &mut document,
                                operation,
                                ancestor,
                                PathMatch::Exact,
                                Vec::new(),
                            )?;
                        }
                    }
                }
            }
        }
        Ok(document)
    }

    /// HTTP 及文件 I/O 复核共用默认规则匹配；不读取内容，也不代替 Run 的审批决策。
    pub(crate) fn resolve(&self, path: &Path, write: bool) -> io::Result<PathBuf> {
        use assistant_runtime::{PermissionEffect, PermissionMatcher, file_matcher_matches};
        let logical = agent_tools::AbsolutePath::new(path.to_owned()).map_err(|_| denied())?;
        let actual = assistant_runtime::resolve_permission_path(logical.as_path())?;
        let physical = agent_tools::AbsolutePath::new(actual.clone()).map_err(|_| denied())?;
        let operation = if write {
            agent_tools::FileOperation::Write
        } else {
            agent_tools::FileOperation::Read
        };
        let document = self.permission_document()?;
        if document.rules.iter().any(|rule| {
            let PermissionMatcher::File(matcher) = &rule.matcher else {
                return false;
            };
            rule.effect == PermissionEffect::Deny
                && (file_matcher_matches(matcher, operation, &logical)
                    || file_matcher_matches(matcher, operation, &physical))
        }) {
            return Err(denied());
        }
        Ok(actual)
    }

    /// 递归搜索从相同的拒绝规则提取排除树，必须在遍历前过滤。
    pub(crate) fn search_exclusions(&self, root: &Path) -> io::Result<Vec<PathBuf>> {
        use assistant_runtime::{PermissionEffect, PermissionFileOperation, PermissionMatcher};
        let mut paths = Vec::new();
        for rule in self.permission_document()?.rules {
            if rule.effect != PermissionEffect::Deny {
                continue;
            }
            let PermissionMatcher::File(matcher) = rule.matcher else {
                continue;
            };
            if matcher.operation != PermissionFileOperation::Search {
                continue;
            }
            let path = assistant_runtime::resolve_permission_path(Path::new(&matcher.path))?;
            if path != root && path.starts_with(root) && !paths.contains(&path) {
                paths.push(path);
            }
        }
        Ok(paths)
    }
}

impl assistant_runtime::PermissionRuleSource for UserPaths {
    fn load(
        &self,
    ) -> assistant_runtime::PermissionStoreFuture<'_, assistant_runtime::PermissionDocument> {
        let paths = self.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || paths.permission_document())
                .await
                .map_err(|_| {
                    assistant_runtime::StoreError::new(
                        assistant_runtime::StoreErrorKind::Unavailable,
                        "default permission task failed",
                    )
                })?
                .map_err(|_| {
                    assistant_runtime::StoreError::new(
                        assistant_runtime::StoreErrorKind::Unavailable,
                        "default permission rules unavailable",
                    )
                })
        })
    }
}

fn push_rule(
    document: &mut assistant_runtime::PermissionDocument,
    operation: assistant_runtime::PermissionFileOperation,
    path: &Path,
    path_match: assistant_runtime::PathMatch,
    excluded_paths: Vec<PathBuf>,
) -> io::Result<()> {
    use assistant_runtime::{
        FilePermissionMatcher, PermissionEffect, PermissionMatcher, PermissionRule,
    };
    let path = path.to_str().ok_or_else(denied)?.to_owned();
    let excluded_paths = excluded_paths
        .into_iter()
        .map(|p| p.to_str().map(str::to_owned).ok_or_else(denied))
        .collect::<io::Result<Vec<_>>>()?;
    document.rules.push(PermissionRule {
        id: format!("default-user-path-{}", document.rules.len()),
        effect: PermissionEffect::Deny,
        variants: vec![
            assistant_protocol::AgentVariant::Plan,
            assistant_protocol::AgentVariant::Build,
        ],
        matcher: PermissionMatcher::File(FilePermissionMatcher {
            operation,
            path,
            path_match,
            excluded_paths,
        }),
    });
    Ok(())
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "此路径不属于当前用户可访问的资源。",
    )
}

#[cfg(test)]
mod tests;
