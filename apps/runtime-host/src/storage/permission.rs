//! 三层权限文件的固定路径解析、revision 与 CAS 原子替换。
//!
//! 权限文件允许用户直接编辑，因此写入不能只依赖进程内锁：Host 用内容 SHA-256 作为
//! revision，写临时文件并在 rename 前再次比较目标 revision。这样既避免半文件，也能发现
//! 读取后到提交前发生的外部编辑。

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use crate::platform;
use assistant_protocol::{AgentVariant, PermissionDiagnosticCode, PermissionScope};
use assistant_runtime::{
    FilePermissionMatcher, GeneralPermissionMatcher, PathMatch, PermissionDocument,
    PermissionEffect, PermissionFileLoad, PermissionFileOperation, PermissionFileRevision,
    PermissionFileScope, PermissionMatcher, PermissionRule, PermissionSourceDiagnostic, StoreError,
    StoreErrorKind, StoredPermissionGrant,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::{
    StorageEngine, StorageResult, conflict, database_write_error, internal_error, invalid_data,
    invalid_data_with_source, sync_directory,
};
use crate::config_source::prepare_private_directory;

const PERMISSION_FILE: &str = "permissions.json";
const CONTROLLER_PERMISSION_FILE: &str = "controller-permissions.json";
const MAX_PERMISSION_FILE_BYTES: u64 = 1024 * 1024;
const DEFAULT_WORKSPACE_RULE_PREFIX: &str = "default-workspace-";
const DEFAULT_SESSION_PRIVATE_RULE_PREFIX: &str = "default-session-private-";
const DEFAULT_SESSION_ATTACHMENT_RULE_PREFIX: &str = "default-session-attachment-";
const DEFAULT_SESSION_MEMORY_RULE_PREFIX: &str = "default-session-memory-";

impl StorageEngine {
    pub(super) fn reconcile_legacy_workspace_permission_file(
        &self,
        workspace: &assistant_runtime::StoredWorkspace,
    ) -> StorageResult<bool> {
        let path = Path::new(&workspace.agent_directory).join(PERMISSION_FILE);
        let loaded = load_path(&path)?;
        let Some(content) = loaded.content.as_deref() else {
            return Ok(false);
        };
        if !loaded.diagnostics.is_empty() {
            return Ok(false);
        }
        let Ok(mut document) = PermissionDocument::parse(content) else {
            // 用户可直接维护权限文件；无法证明为旧系统模板时必须原样保留并交给权限诊断链。
            return Ok(false);
        };
        if !remove_legacy_workspace_default_rules(&mut document) {
            return Ok(false);
        }
        let rendered = document.render().map_err(|source| {
            StoreError::with_source(
                StoreErrorKind::InvalidData,
                "workspace permission migration could not be encoded",
                source,
            )
        })?;
        replace_path(&path, &loaded.revision, &rendered).map(|_| true)
    }

    pub(super) fn ensure_session_permission_file(
        &self,
        environment: &assistant_runtime::SessionExecutionEnvironment,
    ) -> StorageResult<bool> {
        let path = Path::new(&environment.session_private_directory).join(PERMISSION_FILE);
        match fs::symlink_metadata(&path) {
            Ok(_) => return Ok(false),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(internal_error(
                    "session permission metadata could not be read",
                    source,
                ));
            }
        }
        let content = default_session_permission_document(environment)?;
        write_default_permission_file(&path, &content)
    }

    pub(super) fn load_permission_file(
        &self,
        scope: &PermissionFileScope,
    ) -> StorageResult<PermissionFileLoad> {
        let path = self.permission_file_path(scope)?;
        load_path(&path)
    }

    pub(super) fn replace_permission_file(
        &self,
        scope: &PermissionFileScope,
        expected_revision: &PermissionFileRevision,
        content: &[u8],
    ) -> StorageResult<PermissionFileRevision> {
        let path = self.permission_file_path(scope)?;
        replace_path(&path, expected_revision, content)
    }

    pub(super) fn rebase_session_permission_file(
        &self,
        path: &Path,
        old_private: &Path,
        new_private: &Path,
        old_attachment: &Path,
        new_attachment: &Path,
    ) -> StorageResult<()> {
        let loaded = load_path(path)?;
        let Some(content) = loaded.content else {
            return Ok(());
        };
        let Ok(mut document) = PermissionDocument::parse(&content) else {
            // 权限文件允许用户直接编辑。无效文档仍由既有诊断链处理，迁移不能覆盖它。
            return Ok(());
        };
        let mut changed = false;
        for rule in &mut document.rules {
            let PermissionMatcher::File(matcher) = &mut rule.matcher else {
                continue;
            };
            let current = Path::new(&matcher.path);
            let rebased = current
                .strip_prefix(old_private)
                .ok()
                .map(|suffix| rebase_path(new_private, suffix))
                .or_else(|| {
                    current
                        .strip_prefix(old_attachment)
                        .ok()
                        .map(|suffix| rebase_path(new_attachment, suffix))
                });
            let Some(rebased) = rebased else {
                continue;
            };
            matcher.path = rebased
                .to_str()
                .ok_or_else(|| {
                    StoreError::new(
                        StoreErrorKind::InvalidData,
                        "rebased permission path is not valid UTF-8",
                    )
                })?
                .to_owned();
            changed = true;
        }
        if !changed {
            return Ok(());
        }
        let rendered = document.render().map_err(|source| {
            StoreError::with_source(
                StoreErrorKind::InvalidData,
                "rebased permission document could not be encoded",
                source,
            )
        })?;
        replace_path(path, &loaded.revision, &rendered).map(|_| ())
    }

    fn permission_file_path(&self, scope: &PermissionFileScope) -> StorageResult<PathBuf> {
        match scope {
            PermissionFileScope::Global => Ok(self.runtime_home.join(PERMISSION_FILE)),
            PermissionFileScope::Controller => {
                Ok(self.runtime_home.join(CONTROLLER_PERMISSION_FILE))
            }
            PermissionFileScope::Workspace(workspace_id) => {
                let workspace = self.get_workspace(workspace_id)?;
                Ok(Path::new(&workspace.agent_directory).join(PERMISSION_FILE))
            }
            PermissionFileScope::Session(session_id) => {
                let environment = self.load_session_environment(session_id)?;
                Ok(Path::new(&environment.session_private_directory).join(PERMISSION_FILE))
            }
        }
    }

    pub(super) fn list_permission_grants(
        &self,
        include_exhausted: bool,
    ) -> StorageResult<Vec<StoredPermissionGrant>> {
        let sql = if include_exhausted {
            "SELECT grant_id, scope, variants_json, matcher_json, max_uses, remaining_uses,
                    expires_at_ms, note, created_at_ms, consumed_at_ms
             FROM controller_permission_grants
             ORDER BY created_at_ms ASC, grant_id ASC"
        } else {
            "SELECT grant_id, scope, variants_json, matcher_json, max_uses, remaining_uses,
                    expires_at_ms, note, created_at_ms, consumed_at_ms
             FROM controller_permission_grants
             WHERE remaining_uses > 0
             ORDER BY created_at_ms ASC, grant_id ASC"
        };
        let mut statement = self.connection.prepare(sql).map_err(|source| {
            invalid_data_with_source("permission grants are unavailable", source)
        })?;
        let rows = statement
            .query_map([], decode_permission_grant)
            .map_err(|source| {
                invalid_data_with_source("permission grants are unavailable", source)
            })?;
        rows.map(|row| {
            row.map_err(|source| invalid_data_with_source("permission grant is invalid", source))
        })
        .collect()
    }

    pub(super) fn create_permission_grant(
        &mut self,
        grant: StoredPermissionGrant,
    ) -> StorageResult<StoredPermissionGrant> {
        if grant.scope != PermissionScope::Controller
            || grant.max_uses == 0
            || grant.remaining_uses > grant.max_uses
            || grant.created_at_ms < 0
            || grant.expires_at_ms.is_some_and(|exp| exp < 0)
            || grant.consumed_at_ms.is_some_and(|consumed| consumed < 0)
        {
            return Err(StoreError::new(
                StoreErrorKind::InvalidInput,
                "controller permission grant fields are out of bounds",
            ));
        }
        let variants_json = serde_json::to_string(&grant.variants).map_err(|source| {
            StoreError::with_source(
                StoreErrorKind::InvalidInput,
                "permission grant variants could not be encoded",
                source,
            )
        })?;
        let matcher_json = serde_json::to_string(&grant.matcher).map_err(|source| {
            StoreError::with_source(
                StoreErrorKind::InvalidInput,
                "permission grant matcher could not be encoded",
                source,
            )
        })?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("permission grant transaction could not be started", source)
            })?;
        transaction
            .execute(
                "INSERT INTO controller_permission_grants (
                    grant_id, scope, variants_json, matcher_json, max_uses, remaining_uses,
                    expires_at_ms, note, created_at_ms, consumed_at_ms
                 ) VALUES (?1, 'controller', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    grant.grant_id,
                    variants_json,
                    matcher_json,
                    i64::from(grant.max_uses),
                    i64::from(grant.remaining_uses),
                    grant.expires_at_ms,
                    grant.note,
                    grant.created_at_ms,
                    grant.consumed_at_ms,
                ],
            )
            .map_err(|source| {
                database_write_error("controller permission grant could not be created", source)
            })?;
        transaction.commit().map_err(|source| {
            database_write_error("controller permission grant could not be committed", source)
        })?;
        Ok(grant)
    }

    pub(super) fn consume_permission_grant(
        &mut self,
        grant_id: &str,
        now_ms: i64,
    ) -> StorageResult<Option<StoredPermissionGrant>> {
        if now_ms < 0 {
            return Err(StoreError::new(
                StoreErrorKind::InvalidInput,
                "permission grant timestamp must be non-negative",
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("permission grant consume transaction failed", source)
            })?;
        let changed = transaction
            .execute(
                "UPDATE controller_permission_grants
                 SET remaining_uses = remaining_uses - 1,
                     consumed_at_ms = CASE WHEN remaining_uses = 1 THEN ?2 ELSE consumed_at_ms END
                 WHERE grant_id = ?1
                   AND remaining_uses > 0
                   AND (expires_at_ms IS NULL OR expires_at_ms > ?2)",
                params![grant_id, now_ms],
            )
            .map_err(|source| {
                database_write_error("controller permission grant could not be consumed", source)
            })?;
        if changed == 0 {
            transaction.commit().map_err(|source| {
                database_write_error("controller permission grant commit failed", source)
            })?;
            return Ok(None);
        }
        if changed != 1 {
            return Err(conflict("unexpected grant rows modified"));
        }
        let updated = transaction
            .query_row(
                "SELECT grant_id, scope, variants_json, matcher_json, max_uses, remaining_uses,
                        expires_at_ms, note, created_at_ms, consumed_at_ms
                 FROM controller_permission_grants
                 WHERE grant_id = ?1",
                [grant_id],
                decode_permission_grant,
            )
            .optional()
            .map_err(|source| {
                invalid_data_with_source("updated permission grant is invalid", source)
            })?
            .ok_or_else(|| invalid_data("consumed permission grant disappeared"))?;
        transaction.commit().map_err(|source| {
            database_write_error("controller permission grant consume commit failed", source)
        })?;
        Ok(Some(updated))
    }

    pub(super) fn revoke_permission_grant(&mut self, grant_id: &str) -> StorageResult<bool> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("permission grant revoke transaction failed", source)
            })?;
        let changed = transaction
            .execute(
                "DELETE FROM controller_permission_grants WHERE grant_id = ?1",
                [grant_id],
            )
            .map_err(|source| {
                database_write_error("controller permission grant could not be revoked", source)
            })?;
        transaction.commit().map_err(|source| {
            database_write_error("controller permission grant revoke commit failed", source)
        })?;
        Ok(changed > 0)
    }
}

fn decode_permission_grant(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredPermissionGrant> {
    let grant_id: String = row.get(0)?;
    let scope_raw: String = row.get(1)?;
    let scope = match scope_raw.as_str() {
        "controller" => PermissionScope::Controller,
        _ => {
            return Err(to_grant_decode_error(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid permission grant scope",
            )));
        }
    };
    let variants_json: String = row.get(2)?;
    let variants =
        serde_json::from_str::<Vec<AgentVariant>>(&variants_json).map_err(to_grant_decode_error)?;
    let matcher_json: String = row.get(3)?;
    let matcher =
        serde_json::from_str::<PermissionMatcher>(&matcher_json).map_err(to_grant_decode_error)?;
    let max_uses_i64: i64 = row.get(4)?;
    let max_uses = u32::try_from(max_uses_i64).map_err(to_grant_decode_error)?;
    if max_uses == 0 {
        return Err(to_grant_decode_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "permission grant max_uses must be positive",
        )));
    }
    let remaining_uses_i64: i64 = row.get(5)?;
    let remaining_uses = u32::try_from(remaining_uses_i64).map_err(to_grant_decode_error)?;
    if remaining_uses > max_uses {
        return Err(to_grant_decode_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "permission grant remaining_uses exceeds max_uses",
        )));
    }
    let expires_at_ms: Option<i64> = row.get(6)?;
    if expires_at_ms.is_some_and(|exp| exp < 0) {
        return Err(to_grant_decode_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "permission grant expires_at_ms must be non-negative",
        )));
    }
    let note: String = row.get(7)?;
    let created_at_ms: i64 = row.get(8)?;
    if created_at_ms < 0 {
        return Err(to_grant_decode_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "permission grant created_at_ms must be non-negative",
        )));
    }
    let consumed_at_ms: Option<i64> = row.get(9)?;
    if consumed_at_ms.is_some_and(|consumed| consumed < 0) {
        return Err(to_grant_decode_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "permission grant consumed_at_ms must be non-negative",
        )));
    }
    Ok(StoredPermissionGrant {
        grant_id,
        scope,
        variants,
        matcher,
        max_uses,
        remaining_uses,
        expires_at_ms,
        note,
        created_at_ms,
        consumed_at_ms,
    })
}

fn to_grant_decode_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn rebase_path(root: &Path, suffix: &Path) -> PathBuf {
    if suffix.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(suffix)
    }
}

fn replace_path(
    path: &Path,
    expected_revision: &PermissionFileRevision,
    content: &[u8],
) -> StorageResult<PermissionFileRevision> {
    if u64::try_from(content.len()).unwrap_or(u64::MAX) > MAX_PERMISSION_FILE_BYTES {
        return Err(StoreError::new(
            StoreErrorKind::InvalidInput,
            "permission file is too large",
        ));
    }
    // 第一次 CAS 检查快速拒绝以旧 revision 发起的写入。
    let current = load_path(path)?.revision;
    if &current != expected_revision {
        return Err(StoreError::new(
            StoreErrorKind::Conflict,
            "permission file revision changed",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        StoreError::new(
            StoreErrorKind::InvalidData,
            "permission file parent is invalid",
        )
    })?;
    prepare_private_directory(parent).map_err(|source| {
        internal_error("permission file directory could not be prepared", source)
    })?;

    // 临时文件与目标位于同一目录，后续 rename 才能保持同一文件系统内的原子替换语义。
    let (temporary, mut file) = create_temporary_file(parent)?;
    let write_result = (|| -> StorageResult<()> {
        file.write_all(content).map_err(|source| {
            internal_error("permission temporary file could not be written", source)
        })?;
        file.sync_all().map_err(|source| {
            internal_error(
                "permission temporary file could not be synchronized",
                source,
            )
        })?;
        // 文件 I/O 期间用户仍可能保存新内容，所以 rename 前必须再做一次 CAS；
        // 只做入口检查会静默覆盖这段竞态窗口中的外部修改。
        if &load_path(path)?.revision != expected_revision {
            return Err(StoreError::new(
                StoreErrorKind::Conflict,
                "permission file revision changed",
            ));
        }
        fs::rename(&temporary, path)
            .map_err(|source| internal_error("permission file could not be replaced", source))?;
        // rename 持久化的是目录项变化；同步父目录后，成功返回才代表替换已进入耐久边界。
        sync_directory(parent)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result?;
    Ok(revision(content))
}

fn remove_legacy_workspace_default_rules(document: &mut PermissionDocument) -> bool {
    let read_id = format!(
        "{DEFAULT_WORKSPACE_RULE_PREFIX}{}",
        operation_name(PermissionFileOperation::Read)
    );
    let Some(workspace_path) = document.rules.iter().find_map(|rule| {
        if rule.id != read_id {
            return None;
        }
        let PermissionMatcher::File(matcher) = &rule.matcher else {
            return None;
        };
        Some(matcher.path.clone())
    }) else {
        return false;
    };
    let generated =
        default_file_permission_rules(DEFAULT_WORKSPACE_RULE_PREFIX, workspace_path.as_str(), true);
    if !generated
        .iter()
        .all(|expected| document.rules.contains(expected))
    {
        return false;
    }
    // 只有完整七条规则逐字段匹配历史系统模板时才迁移；任一规则被用户修改就全部保留。
    document
        .rules
        .retain(|rule| !generated.iter().any(|expected| expected == rule));
    true
}

fn default_session_permission_document(
    environment: &assistant_runtime::SessionExecutionEnvironment,
) -> StorageResult<Vec<u8>> {
    let mut rules = default_file_permission_rules(
        DEFAULT_SESSION_PRIVATE_RULE_PREFIX,
        &environment.session_private_directory,
        true,
    );
    rules.extend(default_file_permission_rules(
        DEFAULT_SESSION_ATTACHMENT_RULE_PREFIX,
        &environment.session_attachment_directory,
        false,
    ));
    rules.extend(default_memory_permission_rules());
    render_default_permission_document(rules, "default session permissions could not be encoded")
}

fn default_memory_permission_rules() -> Vec<PermissionRule> {
    [
        ("list_pinned_memories", PermissionEffect::Allow),
        ("recall_memory", PermissionEffect::Allow),
        ("pin_memory", PermissionEffect::Ask),
        ("update_pinned_memory", PermissionEffect::Ask),
        ("unpin_memory", PermissionEffect::Ask),
    ]
    .into_iter()
    .map(|(tool_name, effect)| PermissionRule {
        id: format!("{DEFAULT_SESSION_MEMORY_RULE_PREFIX}{tool_name}"),
        effect,
        variants: vec![
            assistant_protocol::AgentVariant::Plan,
            assistant_protocol::AgentVariant::Build,
        ],
        matcher: PermissionMatcher::General(GeneralPermissionMatcher {
            tool_name: tool_name.to_owned(),
        }),
    })
    .collect()
}

fn default_file_permission_rules(
    rule_prefix: &str,
    path: &str,
    include_mutations: bool,
) -> Vec<PermissionRule> {
    let operations = [
        PermissionFileOperation::Read,
        PermissionFileOperation::List,
        PermissionFileOperation::Find,
        PermissionFileOperation::Search,
        PermissionFileOperation::Write,
        PermissionFileOperation::Edit,
        PermissionFileOperation::Delete,
    ];
    operations
        .into_iter()
        .filter(|operation| include_mutations || !is_mutation(*operation))
        .map(|operation| PermissionRule {
            id: format!("{rule_prefix}{}", operation_name(operation)),
            effect: PermissionEffect::Allow,
            variants: if is_mutation(operation) {
                vec![assistant_protocol::AgentVariant::Build]
            } else {
                vec![
                    assistant_protocol::AgentVariant::Plan,
                    assistant_protocol::AgentVariant::Build,
                ]
            },
            matcher: PermissionMatcher::File(FilePermissionMatcher {
                excluded_paths: Vec::new(),
                operation,
                path: path.to_owned(),
                path_match: PathMatch::Recursive,
            }),
        })
        .collect()
}

fn render_default_permission_document(
    rules: Vec<PermissionRule>,
    message: &'static str,
) -> StorageResult<Vec<u8>> {
    PermissionDocument {
        schema_version: 1,
        rules,
    }
    .render()
    .map_err(|source| StoreError::with_source(StoreErrorKind::Internal, message, source))
}

fn is_mutation(operation: PermissionFileOperation) -> bool {
    matches!(
        operation,
        PermissionFileOperation::Write
            | PermissionFileOperation::Edit
            | PermissionFileOperation::Delete
    )
}

fn operation_name(operation: PermissionFileOperation) -> &'static str {
    match operation {
        PermissionFileOperation::Read => "read",
        PermissionFileOperation::List => "list",
        PermissionFileOperation::Find => "find",
        PermissionFileOperation::Search => "search",
        PermissionFileOperation::Write => "write",
        PermissionFileOperation::Edit => "edit",
        PermissionFileOperation::Delete => "delete",
    }
}

fn write_default_permission_file(path: &Path, content: &[u8]) -> StorageResult<bool> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    platform::apply_private_open_options(&mut options);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(source) => {
            return Err(internal_error(
                "default permission file could not be created",
                source,
            ));
        }
    };
    let result = (|| -> StorageResult<()> {
        file.write_all(content).map_err(|source| {
            internal_error("default permission file could not be written", source)
        })?;
        file.sync_all().map_err(|source| {
            internal_error("default permission file could not be synchronized", source)
        })?;
        let parent = path.parent().ok_or_else(|| {
            StoreError::new(
                StoreErrorKind::InvalidData,
                "permission file parent is invalid",
            )
        })?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result.map(|()| true)
}

fn load_path(path: &Path) -> StorageResult<PermissionFileLoad> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(PermissionFileLoad {
                content: None,
                revision: PermissionFileRevision::Missing,
                diagnostics: Vec::new(),
            });
        }
        Err(source) => {
            return Err(internal_error(
                "permission file metadata could not be read",
                source,
            ));
        }
    };
    if !metadata.file_type().is_file() {
        return Err(StoreError::new(
            StoreErrorKind::InvalidData,
            "permission path is not a regular file",
        ));
    }
    if metadata.len() > MAX_PERMISSION_FILE_BYTES {
        return Err(StoreError::new(
            StoreErrorKind::InvalidData,
            "permission file is too large",
        ));
    }
    let content = fs::read(path)
        .map_err(|source| internal_error("permission file could not be read", source))?;
    let diagnostics = if platform::private_file_mode_is_secured(&metadata) {
        Vec::new()
    } else {
        vec![PermissionSourceDiagnostic {
            code: PermissionDiagnosticCode::UnsafePermissions,
            message: "permission file is accessible beyond the current user",
        }]
    };
    Ok(PermissionFileLoad {
        revision: revision(&content),
        content: Some(content),
        diagnostics,
    })
}

fn revision(content: &[u8]) -> PermissionFileRevision {
    PermissionFileRevision::Content(format!("{:x}", Sha256::digest(content)))
}

fn create_temporary_file(parent: &Path) -> StorageResult<(PathBuf, fs::File)> {
    for _ in 0..16 {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).map_err(|source| {
            internal_error("permission temporary name could not be generated", source)
        })?;
        let path = parent.join(format!(".permissions-{}.tmp", hex(&random)));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        platform::apply_private_open_options(&mut options);
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(internal_error(
                    "permission temporary file could not be created",
                    source,
                ));
            }
        }
    }
    Err(StoreError::new(
        StoreErrorKind::Internal,
        "permission temporary name could not be allocated",
    ))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(char::from(DIGITS[usize::from(byte >> 4)]));
        value.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    value
}
