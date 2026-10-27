//! 同版本 F02 的文件准备与原子指针切换。准备件不发布、不删除旧文件；失败保留旧权威。

#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use assistant_protocol::{ChildTaskId, ConversationOwner, SessionId};
use rusqlite::{Connection, params};

use super::{Migration, MigrationError, Result, backup, ledger_is_keyed, v0_30_0};
use crate::{
    host_layout::files::{self, Entry},
    storage::{StorageEngine, body_path, child_body_path, child_task_directory, conversation},
};

pub(super) const KEY: &str = "f02_history_v1";
const CATALOGUE_BATCH_SIZE: i64 = 64;

pub(super) fn entry() -> Migration {
    Migration {
        key: KEY,
        version: "0.32.1",
        min_compatible_host_version: Some("0.32.1"),
        rebuilds_referenced_tables: true,
        validate_source: v0_30_0::validate,
        apply: apply_schema,
        validate,
    }
}

pub(super) fn committed(connection: &Connection) -> Result<bool> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='schema_migrations')",
        [],
        |row| row.get(0),
    )?;
    if !exists || !ledger_is_keyed(connection)? {
        return Ok(false);
    }
    Ok(connection.query_row("SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version='0.32.1' AND migration_key=?1)", [KEY], |row| row.get(0))?)
}

struct PreparedOwner {
    owner: ConversationOwner,
    source_generation: u64,
    target_generation: u64,
    message_count: u64,
    source_path: PathBuf,
    source_file: Entry,
    target_path: PathBuf,
    target_file: Entry,
}

/// 仅含复核证据和未发布文件的切换清单；不保留整库或所有会话正文。
pub(super) struct Prepared {
    source: backup::DatabaseEvidence,
    owners: Vec<PreparedOwner>,
    materialization_inputs: Vec<(SessionId, assistant_protocol::InputId)>,
}

/// 调用者持有实例锁且已核验配套备份。旧恢复可能独立提交；之后的失败不会撤销这些恢复。
#[cfg(test)]
pub(super) fn prepare(connection: Connection, home: &Path) -> Result<(Connection, Prepared)> {
    prepare_with_progress(connection, home, |_, _| {})
}

pub(super) fn prepare_with_progress(
    connection: Connection,
    home: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<(Connection, Prepared)> {
    let mut engine = StorageEngine::for_history_upgrade(home, connection)?;
    engine.recover_session_deletions()?;
    engine.recover_session_history_operations()?;
    if !engine.recover_body_appends()?.is_empty() {
        return Err(MigrationError::Integrity);
    }
    if !engine.recover_child_storage()?.is_empty() {
        return Err(MigrationError::Integrity);
    }
    engine.interrupt_nonterminal_child_tasks()?;
    if !engine.recover_pending_tool_exchanges()?.is_empty() {
        return Err(MigrationError::Integrity);
    }
    // 只闭合已有终态 Run 自己的 queued Input，不执行模型、工具或恢复排队工作。
    // 仅冻结需要收敛的定位键，不装载全库 Run 或所有未消费正文。
    loop {
        let pending_inputs = engine
            .connection
            .prepare(
                "SELECT r.run_id,r.session_id,r.input_id,COALESCE(r.finished_at_ms,r.created_at_ms)
         FROM runs r JOIN inputs i ON i.input_id=r.input_id AND i.session_id=r.session_id
         WHERE r.status IN ('completed','failed','cancelled','interrupted') AND i.state='queued'
         ORDER BY r.session_id,r.run_id LIMIT ?1",
            )?
            .query_map([CATALOGUE_BATCH_SIZE], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if pending_inputs.is_empty() {
            break;
        }
        for (run_id, session_id, input_id, recovered_at_ms) in pending_inputs {
            let run_id =
                assistant_protocol::RunId::new(run_id).map_err(|_| MigrationError::Integrity)?;
            let session_id = SessionId::new(session_id).map_err(|_| MigrationError::Integrity)?;
            let input_id = assistant_protocol::InputId::new(input_id)
                .map_err(|_| MigrationError::Integrity)?;
            let input = engine
                .load_input_for_id(&session_id, &input_id)?
                .ok_or(MigrationError::Integrity)?;
            if input.state == assistant_runtime::StoredInputState::Queued {
                engine.reconcile_terminal_run_input(
                    assistant_runtime::StoredTerminalRunInputReconciliation {
                        operation_id: format!("f02-reconcile:{}", run_id.as_str()),
                        session_id,
                        run_id,
                        input_id,
                        user_message: input.queued_message.ok_or(MigrationError::Integrity)?,
                        recovered_at_ms,
                    },
                )?;
            }
        }
    }
    let source = backup::evidence(&engine.connection)?;
    // 旧格式没有显式原 Input 绑定。只有从未换代且符合原物化提交身份的首条输入可证明；
    // 已清空／重写等旧会话保留 key 和未知状态，不从当前队首猜造原身份。
    let materialization_inputs = engine.connection.prepare(
        "SELECT s.session_id,i.input_id FROM sessions s JOIN inputs i ON i.session_id=s.session_id
         WHERE s.materialization_key IS NOT NULL AND s.body_generation=1
           AND i.input_kind='message' AND i.origin='user' AND i.accepted_at_ms=s.created_at_ms
           AND i.queue_order=(SELECT MIN(x.queue_order) FROM inputs x WHERE x.session_id=s.session_id AND x.input_kind='message')
           AND EXISTS(SELECT 1 FROM runs r WHERE r.session_id=s.session_id AND r.input_id=i.input_id
               AND r.attempt=1 AND r.created_at_ms=s.created_at_ms)"
    )?.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?
        .map(|row| {
            let (session,input) = row?;
            Ok((SessionId::new(session).map_err(|_|MigrationError::Integrity)?,assistant_protocol::InputId::new(input).map_err(|_|MigrationError::Integrity)?))
        }).collect::<Result<Vec<_>>>()?;
    let mut owners = Vec::new();
    // 文件迁出只依赖 catalogue；旧版本尚待正常启动补齐的执行环境不能成为历史迁出的前置。
    // 不在升级期间装配 Session、冻结提示词或工具环境。
    // 先冻结每个 child 的目标代次，父文件中的只读 child 定位也随同一次切换更新。
    // 全库只保留最小目标定位；目录分批读取，不装配所有 child 的请求、配置或执行对象。
    let mut targets = HashMap::new();
    let mut total = 0;
    let mut after = None;
    loop {
        let batch = owner_catalogue_batch(&engine.connection, after.as_ref())?;
        if batch.is_empty() {
            break;
        }
        for (owner, generation, _) in batch {
            total += 1;
            if let ConversationOwner::ChildTask {
                session_id,
                child_task_id,
            } = &owner
            {
                targets.insert(
                    (session_id.clone(), child_task_id.clone()),
                    next_generation(home, &owner, generation)?,
                );
            }
            after = Some(owner);
        }
    }
    progress(0, total);
    let mut after = None;
    loop {
        let batch = owner_catalogue_batch(&engine.connection, after.as_ref())?;
        if batch.is_empty() {
            break;
        }
        for (owner, generation, message_count) in batch {
            let target = match &owner {
                ConversationOwner::MainSession { .. } => next_generation(home, &owner, generation)?,
                ConversationOwner::ChildTask {
                    session_id,
                    child_task_id,
                } => targets[&(session_id.clone(), child_task_id.clone())],
            };
            after = Some(owner.clone());
            owners.push(prepare_owner(
                &mut engine,
                home,
                owner,
                generation,
                target,
                message_count,
                &targets,
            )?);
            progress(owners.len() as u64, total);
        }
    }
    Ok((
        engine.connection,
        Prepared {
            source,
            owners,
            materialization_inputs,
        },
    ))
}

/// 升级持实例锁，源目录在准备期间不变；按完整 owner 游标读取有界批次。
/// 这里只读取文件定位和数量，不解码业务执行载荷；全部文件核验后仍沿原单事务切换。
fn owner_catalogue_batch(
    connection: &Connection,
    after: Option<&ConversationOwner>,
) -> Result<Vec<(ConversationOwner, u64, u64)>> {
    let (kind, session, child) = match after {
        None => (0, "", ""),
        Some(ConversationOwner::MainSession { session_id }) => (0, session_id.as_str(), ""),
        Some(ConversationOwner::ChildTask {
            session_id,
            child_task_id,
        }) => (1, session_id.as_str(), child_task_id.as_str()),
    };
    connection.prepare(
        "SELECT owner_kind,session_id,child_task_id,body_generation,message_count FROM (
         SELECT 0 AS owner_kind,session_id,'' AS child_task_id,body_generation,message_count FROM sessions
         UNION ALL SELECT 1,session_id,child_task_id,body_generation,message_count FROM child_tasks)
         WHERE (owner_kind,session_id,child_task_id)>(?1,?2,?3)
         ORDER BY owner_kind,session_id,child_task_id LIMIT ?4"
    )?.query_map(params![kind, session, child, CATALOGUE_BATCH_SIZE], |row| Ok((
        row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
        row.get::<_, i64>(3)?, row.get::<_, i64>(4)?
    )))?.map(|row| {
        let (kind, session, child, generation, count) = row?;
        let session_id = SessionId::new(session).map_err(|_| MigrationError::Integrity)?;
        let owner = match kind {
            0 => ConversationOwner::MainSession { session_id },
            1 => ConversationOwner::ChildTask { session_id, child_task_id: ChildTaskId::new(child).map_err(|_| MigrationError::Integrity)? },
            _ => return Err(MigrationError::Integrity),
        };
        let generation = u64::try_from(generation).ok().filter(|generation| *generation > 0).ok_or(MigrationError::Integrity)?;
        let count = u64::try_from(count).map_err(|_| MigrationError::Integrity)?;
        Ok((owner, generation, count))
    }).collect()
}

fn prepare_owner(
    engine: &mut StorageEngine,
    home: &Path,
    owner: ConversationOwner,
    generation: u64,
    target_generation: u64,
    message_count: u64,
    child_targets: &HashMap<(SessionId, ChildTaskId), u64>,
) -> Result<PreparedOwner> {
    let source_path = owner_path(home, &owner, generation)?;
    check_managed_ancestors(home, &source_path)?;
    let source_file = files::inspect(&source_path).map_err(file_error)?;
    if !matches!(source_file, Entry::File { .. }) {
        return Err(MigrationError::InvalidPath);
    }
    let (keys, source_history) = conversation::scan_document(&source_path, &owner, generation)?;
    if u64::try_from(keys.messages.len()).ok() != Some(message_count) {
        return Err(MigrationError::Integrity);
    }
    let mut history = source_history.unwrap_or_default();
    let mut legacy = engine.legacy_history_facts_for_keys(&owner, &keys)?;
    // 旧 SQL 的可靠终态补齐尚未带元数据的旧快捷路径；已知文件 Step 不被旧空值覆盖。
    let known_origins: std::collections::HashSet<_> = history
        .message_origins
        .iter()
        .map(|origin| origin.message_id.clone())
        .collect();
    legacy
        .message_origins
        .retain(|origin| !known_origins.contains(&origin.message_id));
    history.extend(legacy);
    history.normalize();
    let session_id = match &owner {
        ConversationOwner::MainSession { session_id }
        | ConversationOwner::ChildTask { session_id, .. } => session_id,
    };
    for child in &mut history.child_tasks {
        if let Some(target) = child_targets.get(&(session_id.clone(), child.child_task_id.clone()))
        {
            child.body_generation = *target;
        }
    }
    let target = owner_path(home, &owner, target_generation)?;
    conversation::write_upgraded_generation(
        &source_path,
        &target,
        &owner,
        generation,
        target_generation,
        message_count,
        &history,
    )?;
    crate::storage::sync_directory(target.parent().ok_or(MigrationError::InvalidPath)?)?;
    if files::inspect(&source_path).map_err(file_error)? != source_file {
        return Err(MigrationError::DatabaseChanged);
    }
    Ok(PreparedOwner {
        owner,
        source_generation: generation,
        target_generation,
        message_count,
        source_path,
        source_file,
        target_file: files::inspect(&target).map_err(file_error)?,
        target_path: target,
    })
}

impl Prepared {
    /// schema 增加最小身份列后，在同一切换事务绑定已核实的原接纳；不保存历史结果。
    pub(super) fn bind_materialization_inputs(&self, connection: &Connection) -> Result<()> {
        for (session, input) in &self.materialization_inputs {
            if connection.execute("UPDATE sessions SET materialization_input_id=?1 WHERE session_id=?2 AND materialization_key IS NOT NULL AND materialization_input_id IS NULL",
                params![input.as_str(),session.as_str()])? != 1 {
                return Err(MigrationError::DatabaseChanged);
            }
        }
        Ok(())
    }

    /// 在账本桥接和任何 schema／指针写入前复核同一冻结源。
    pub(super) fn verify(&self, connection: &Connection) -> Result<()> {
        if backup::evidence(connection)? != self.source {
            return Err(MigrationError::DatabaseChanged);
        }
        for owner in &self.owners {
            if files::inspect(&owner.source_path).map_err(file_error)? != owner.source_file {
                return Err(MigrationError::DatabaseChanged);
            }
            if files::inspect(&owner.target_path).map_err(file_error)? != owner.target_file {
                return Err(MigrationError::DatabaseChanged);
            }
        }
        Ok(())
    }

    pub(super) fn switch(&self, connection: &Connection) -> Result<()> {
        for owner in &self.owners {
            let target_generation =
                i64::try_from(owner.target_generation).map_err(|_| MigrationError::Integrity)?;
            let source_generation =
                i64::try_from(owner.source_generation).map_err(|_| MigrationError::Integrity)?;
            let message_count =
                i64::try_from(owner.message_count).map_err(|_| MigrationError::Integrity)?;
            let changed = match &owner.owner {
                ConversationOwner::MainSession { session_id } => connection.execute(
                    "UPDATE sessions SET body_generation=?1 WHERE session_id=?2 AND body_generation=?3 AND message_count=?4",
                    params![target_generation, session_id.as_str(), source_generation, message_count],
                )?,
                ConversationOwner::ChildTask { session_id, child_task_id } => connection.execute(
                    "UPDATE child_tasks SET body_generation=?1 WHERE child_task_id=?2 AND session_id=?3 AND body_generation=?4 AND message_count=?5",
                    params![target_generation, child_task_id.as_str(), session_id.as_str(), source_generation, message_count],
                )?,
            };
            if changed != 1 {
                return Err(MigrationError::DatabaseChanged);
            }
        }
        Ok(())
    }
}

fn owner_path(home: &Path, owner: &ConversationOwner, generation: u64) -> Result<PathBuf> {
    let session = match owner {
        ConversationOwner::MainSession { session_id }
        | ConversationOwner::ChildTask { session_id, .. } => session_id,
    };
    crate::storage::filesystem::validate_session_component(session)?;
    let directory = home.join("data/sessions").join(session.as_str());
    Ok(match owner {
        ConversationOwner::MainSession { .. } => body_path(&directory, generation),
        ConversationOwner::ChildTask { child_task_id, .. } => {
            crate::storage::filesystem::validate_child_task_component(child_task_id)?;
            child_body_path(&child_task_directory(&directory, child_task_id), generation)
        }
    })
}

fn next_generation(home: &Path, owner: &ConversationOwner, source: u64) -> Result<u64> {
    let mut generation = source.checked_add(1).ok_or(MigrationError::Integrity)?;
    while files::exists(&owner_path(home, owner, generation)?).map_err(file_error)? {
        generation = generation.checked_add(1).ok_or(MigrationError::Integrity)?;
    }
    Ok(generation)
}

fn check_managed_ancestors(home: &Path, file: &Path) -> Result<()> {
    let relative = file
        .strip_prefix(home)
        .map_err(|_| MigrationError::InvalidPath)?;
    let mut path = home.to_path_buf();
    for component in relative
        .parent()
        .ok_or(MigrationError::InvalidPath)?
        .components()
    {
        path.push(component);
        if files::inspect(&path).map_err(file_error)? != Entry::Directory {
            return Err(MigrationError::InvalidPath);
        }
    }
    Ok(())
}

fn file_error(source: crate::host_layout::Error) -> MigrationError {
    MigrationError::Store(crate::storage::internal_error(
        "history upgrade file verification failed",
        source,
    ))
}

fn apply_schema(connection: &Connection) -> Result<()> {
    // 活动控制不应携带终态字段；未知旧状态不能在删列时被静默裁掉，完整源备份仍保留。
    if connection.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE status IN ('accepted','running','cancelling')
        AND (error_code IS NOT NULL OR error_message IS NOT NULL OR finished_at_ms IS NOT NULL))
        OR EXISTS(SELECT 1 FROM child_tasks WHERE status IN ('accepted','running')
        AND (final_message_id IS NOT NULL OR error_code IS NOT NULL OR error_message IS NOT NULL OR finished_at_ms IS NOT NULL))",
        [],|row|row.get::<_,bool>(0))? { return Err(MigrationError::Integrity); }
    connection.execute_batch(include_str!("f02_inputs.sql"))?;
    connection.execute_batch(include_str!("f02_history.sql"))?;
    Ok(())
}

pub(super) fn validate(connection: &Connection) -> Result<()> {
    // 当前 schema 明确移除 SQL 历史表；复用保留布局的各版本规则，不放宽旧源准入。
    if connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name IN ('run_message_refs','message_feedback'))",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(MigrationError::Integrity);
    }
    super::v0_25_1::validate_retained(connection)?;
    for (table, columns) in [
        (
            "runs",
            &["error_code", "error_message", "finished_at_ms"][..],
        ),
        (
            "child_tasks",
            &[
                "final_message_id",
                "error_code",
                "error_message",
                "finished_at_ms",
            ][..],
        ),
    ] {
        for column in columns {
            if connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
                params![table, column],
                |row| row.get::<_, bool>(0),
            )? {
                return Err(MigrationError::Integrity);
            }
        }
    }
    if connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('model_request_records') WHERE name='run_id')",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(MigrationError::Integrity);
    }
    let checks: [fn(&Connection) -> Result<()>; 7] = [
        super::v0_25_3::validate_additions,
        super::v0_26_0::validate_additions,
        super::v0_27_0::validate_additions,
        super::v0_28_0::validate_additions,
        super::v0_28_1::validate_additions,
        super::v0_29_0::validate_layout_additions,
        super::v0_30_0::validate_additions,
    ];
    for check in checks {
        check(connection)?;
    }
    // 完成命令的 SQL 行仅作幂等身份；旧布局或被重新写入的历史载荷不能通过准入。
    for name in [
        "user_message_id",
        "accepted_at_ms",
        "agent_variant",
        "origin",
    ] {
        if connection.query_row(
            "SELECT \"notnull\" FROM pragma_table_info('inputs') WHERE name=?1",
            [name],
            |row| row.get::<_, bool>(0),
        )? {
            return Err(MigrationError::Integrity);
        }
    }
    if connection.query_row("SELECT EXISTS(SELECT 1 FROM inputs i WHERE i.input_kind='message' AND i.state='committed'
        AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.input_id=i.input_id AND r.session_id=i.session_id AND r.status IN ('accepted','running','cancelling'))
        AND (i.user_message_id IS NOT NULL OR i.accepted_at_ms IS NOT NULL OR i.agent_variant IS NOT NULL OR i.origin IS NOT NULL
            OR i.priority_order IS NOT NULL OR i.goal_id IS NOT NULL OR i.goal_generation IS NOT NULL OR i.goal_turn IS NOT NULL
            OR i.goal_reply_route_json IS NOT NULL OR i.skill_activation_json IS NOT NULL OR i.cross_session_json IS NOT NULL
            OR i.channel_source_json IS NOT NULL OR i.agent_shell_target IS NOT NULL OR i.queued_message_json IS NOT NULL))",
        [],|row|row.get::<_,bool>(0))? { return Err(MigrationError::Integrity); }
    if connection.query_row("SELECT EXISTS(SELECT 1 FROM inputs WHERE input_kind='command' AND state='committed'
        AND (priority_order IS NOT NULL OR user_message_id IS NOT NULL OR command_json IS NOT NULL
            OR command_result_json IS NOT NULL OR accepted_at_ms IS NOT NULL OR agent_variant IS NOT NULL OR origin IS NOT NULL))",
        [], |row| row.get::<_, bool>(0))? {
        return Err(MigrationError::Integrity);
    }
    for table in ["skill_activations", "mcp_input_selections"] {
        if connection.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {table} a WHERE NOT EXISTS(
             SELECT 1 FROM inputs i WHERE i.input_id=a.input_id
             AND i.session_id=a.session_id AND i.state='queued'))"
            ),
            [],
            |row| row.get::<_, bool>(0),
        )? {
            return Err(MigrationError::Integrity);
        }
    }
    if connection.query_row("SELECT EXISTS(SELECT 1 FROM inputs WHERE state='committed' AND skill_activation_json IS NOT NULL)", [], |row| row.get::<_, bool>(0))? {
        return Err(MigrationError::Integrity);
    }
    connection.prepare("SELECT materialization_input_id FROM sessions LIMIT 0")?;
    if connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM runs WHERE status NOT IN ('accepted','running','cancelling'))",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(MigrationError::Integrity);
    }
    if connection.query_row(
        "SELECT COUNT(*) FROM child_tasks WHERE status NOT IN ('accepted','running')",
        [],
        |row| row.get::<_, i64>(0),
    )? != 0
    {
        return Err(MigrationError::Integrity);
    }
    if connection.query_row("SELECT COUNT(*) FROM session_feedback WHERE json_extract(snapshot_json,'$.status') IS NOT 'pending'", [], |row| row.get::<_,i64>(0))? != 0 {
        return Err(MigrationError::Integrity);
    }
    connection.prepare("SELECT delegated_child_json FROM pending_tool_starts LIMIT 0")?;
    let mut keys = connection
        .prepare("PRAGMA table_info(conversation_recall_heads)")?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(5)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    keys.retain(|(position, _)| *position > 0);
    keys.sort_by_key(|(position, _)| *position);
    if keys.into_iter().map(|(_, name)| name).collect::<Vec<_>>()
        != ["session_id", "owner_kind", "owner_id"]
    {
        return Err(MigrationError::Integrity);
    }
    for table in [
        "conversation_recall_documents",
        "conversation_recall_heads",
        "session_feedback",
    ] {
        let foreign_keys: Vec<String> = connection
            .prepare(&format!("PRAGMA foreign_key_list({table})"))?
            .query_map([], |row| row.get(2))?
            .collect::<rusqlite::Result<_>>()?;
        if foreign_keys != ["sessions"] {
            return Err(MigrationError::Integrity);
        }
    }
    for table in ["body_appends", "child_body_appends"] {
        let sql: String = connection.query_row(
            "SELECT sql FROM sqlite_schema WHERE name=?1 AND type='table'",
            [table],
            |row| row.get(0),
        )?;
        let compact: String = sql
            .chars()
            .filter(|value| !value.is_ascii_whitespace())
            .collect();
        if !compact.contains("message_count_delta>=0") {
            return Err(MigrationError::Integrity);
        }
    }
    Ok(())
}

/// 将旧版“排队即建 Run”的未开始控制还原为纯暂存 Input。
/// 仍由统一执行器先备份并核验，再在同一事务应用并登记；不处理真正开始或待提交的执行。
pub(super) fn queue_claim_entry() -> Migration {
    Migration {
        key: "f02_queue_claim_v1",
        version: "0.32.1",
        min_compatible_host_version: Some("0.32.1"),
        rebuilds_referenced_tables: false,
        validate_source: validate,
        apply: detach_unclaimed_runs,
        validate,
    }
}

fn detach_unclaimed_runs(connection: &Connection) -> Result<()> {
    // accepted + 正文回执是已经领取但尚未完成开始提交的执行，保留给 staged append 恢复。
    // 只有完全没有执行证据的旧排队控制可以解除；不一致的工具/child 控制拒绝迁移。
    let invalid: i64 = connection.query_row(
        "SELECT COUNT(*) FROM runs r JOIN inputs i USING(input_id)
         WHERE i.state='queued' AND r.status='accepted' AND (
             r.attempt!=1 OR r.started_at_ms IS NOT NULL OR
             EXISTS(SELECT 1 FROM pending_tool_exchanges p WHERE p.run_id=r.run_id) OR
             EXISTS(SELECT 1 FROM child_tasks c WHERE c.parent_run_id=r.run_id))",
        [],
        |row| row.get(0),
    )?;
    if invalid != 0 {
        return Err(MigrationError::InvalidLedger);
    }
    let unclaimed = "SELECT r.run_id FROM runs r JOIN inputs i USING(input_id)
        WHERE i.state='queued' AND r.status='accepted'
        AND NOT EXISTS(SELECT 1 FROM body_appends b WHERE b.run_id=r.run_id)";
    let expected: i64 =
        connection.query_row(&format!("SELECT COUNT(*) FROM ({unclaimed})"), [], |row| {
            row.get(0)
        })?;
    connection.execute(&format!("UPDATE inputs SET skill_activation_json=json_set(skill_activation_json,'$.run_id',NULL)
        WHERE skill_activation_json IS NOT NULL AND input_id IN(SELECT input_id FROM runs WHERE run_id IN ({unclaimed}))"),[])?;
    connection.execute(
        &format!("UPDATE skill_activations SET run_id=NULL WHERE run_id IN ({unclaimed})"),
        [],
    )?;
    let removed = connection.execute(
        &format!("DELETE FROM runs WHERE run_id IN ({unclaimed})"),
        [],
    )?;
    if i64::try_from(removed).ok() != Some(expected) {
        return Err(MigrationError::DatabaseChanged);
    }
    Ok(())
}
