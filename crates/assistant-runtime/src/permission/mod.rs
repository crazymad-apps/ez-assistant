//! 分层权限文件的领域模型、基础设施端口与内存快照。
//!
//! JSON 文件是唯一规则源；Registry 只保存已完整校验的不可变快照。

mod approval;
mod authorizer;
mod document;
mod matcher;
mod projection;
mod registry;

use std::sync::atomic::{AtomicU64, Ordering};
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Mutex};

use assistant_protocol::{PermissionDiagnosticCode, PermissionScope, SessionId, WorkspaceId};

use crate::StoreError;

pub(crate) use approval::{ApprovalRegistry, RuntimeApprovalResolver, rules_for_approval};
pub(crate) use authorizer::{RunAuthorizationScope, RuntimeToolAuthorizer};
pub use document::{
    CommandMatch, FilePermissionMatcher, GeneralPermissionMatcher, PathMatch, PermissionDocument,
    PermissionDocumentError, PermissionEffect, PermissionFileOperation, PermissionMatcher,
    PermissionProcessMode, PermissionRule, ShellPermissionMatcher,
};
pub(crate) use matcher::{file_matcher_matches, is_exact_allow_rule, matches_rule};
pub(crate) use projection::{
    document_from_protocol, revision_from_protocol, scope_from_protocol, snapshot_from_load,
};
pub(crate) use registry::PermissionCoordinator;

/// Host 可定位的权限文件作用域。
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PermissionFileScope {
    Global,
    Workspace(WorkspaceId),
    Session(SessionId),
}

impl PermissionFileScope {
    pub fn level(&self) -> PermissionScope {
        match self {
            Self::Global => PermissionScope::Global,
            Self::Workspace(_) => PermissionScope::Workspace,
            Self::Session(_) => PermissionScope::Session,
        }
    }
}

/// Host 基于原始文件字节生成的不透明 revision。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PermissionFileRevision {
    Missing,
    Content(String),
}

/// Host 在不解析业务 JSON 的前提下发现的安全诊断。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionSourceDiagnostic {
    pub code: PermissionDiagnosticCode,
    pub message: &'static str,
}

/// 单份权限文件的原始加载结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionFileLoad {
    pub content: Option<Vec<u8>>,
    pub revision: PermissionFileRevision,
    pub diagnostics: Vec<PermissionSourceDiagnostic>,
}

/// 权限文件端口的统一 Future。
pub type PermissionStoreFuture<'a, Output> =
    Pin<Box<dyn Future<Output = Result<Output, StoreError>> + Send + 'a>>;

/// Runtime 对权限文件所需的最小基础设施能力。
pub trait PermissionFileStore: Send + Sync {
    fn load_permission_file(
        &self,
        scope: &PermissionFileScope,
    ) -> PermissionStoreFuture<'_, PermissionFileLoad>;

    fn replace_permission_file(
        &self,
        scope: &PermissionFileScope,
        expected_revision: &PermissionFileRevision,
        content: Vec<u8>,
    ) -> PermissionStoreFuture<'_, PermissionFileRevision>;
}

/// 嵌入式 Runtime 与单元测试使用的易失权限文件端口。
pub(crate) struct VolatilePermissionFileStore {
    files: Mutex<BTreeMap<PermissionFileScope, (PermissionFileRevision, Vec<u8>)>>,
    next_revision: AtomicU64,
}

impl Default for VolatilePermissionFileStore {
    fn default() -> Self {
        Self {
            files: Mutex::new(BTreeMap::new()),
            next_revision: AtomicU64::new(1),
        }
    }
}

impl PermissionFileStore for VolatilePermissionFileStore {
    fn load_permission_file(
        &self,
        scope: &PermissionFileScope,
    ) -> PermissionStoreFuture<'_, PermissionFileLoad> {
        let result = self
            .files
            .lock()
            .map_err(|_| {
                StoreError::new(
                    crate::StoreErrorKind::Unavailable,
                    "volatile permission store is unavailable",
                )
            })
            .map(|files| match files.get(scope) {
                Some((revision, content)) => PermissionFileLoad {
                    content: Some(content.clone()),
                    revision: revision.clone(),
                    diagnostics: Vec::new(),
                },
                None => PermissionFileLoad {
                    content: None,
                    revision: PermissionFileRevision::Missing,
                    diagnostics: Vec::new(),
                },
            });
        Box::pin(async move { result })
    }

    fn replace_permission_file(
        &self,
        scope: &PermissionFileScope,
        expected_revision: &PermissionFileRevision,
        content: Vec<u8>,
    ) -> PermissionStoreFuture<'_, PermissionFileRevision> {
        let result = self
            .files
            .lock()
            .map_err(|_| {
                StoreError::new(
                    crate::StoreErrorKind::Unavailable,
                    "volatile permission store is unavailable",
                )
            })
            .and_then(|mut files| {
                let current = files
                    .get(scope)
                    .map(|(revision, _)| revision.clone())
                    .unwrap_or(PermissionFileRevision::Missing);
                if &current != expected_revision {
                    return Err(StoreError::new(
                        crate::StoreErrorKind::Conflict,
                        "permission file revision changed",
                    ));
                }
                let revision = PermissionFileRevision::Content(format!(
                    "volatile-{}",
                    self.next_revision.fetch_add(1, Ordering::Relaxed)
                ));
                files.insert(scope.clone(), (revision.clone(), content));
                Ok(revision)
            });
        Box::pin(async move { result })
    }
}
