//! v1 Input 接纳身份随文件代次切换迁入 v2；保留实际队列及未完成执行控制。

pub(super) mod sidebar_order;

use std::{collections::HashMap, path::Path};

use assistant_protocol::{ChildTaskId, ConversationOwner, IdempotencyKey, SessionId};
use assistant_runtime::StoredHistoryFacts;
use rusqlite::Connection;

use super::{Migration, MigrationError, Result, backup, f02_history, v0_33_0};
use crate::{
    host_layout::files::{self, Entry},
    storage::{StorageEngine, conversation, history::HistoryMessageKeys},
};

pub(super) const KEY: &str = "release";

pub(super) fn entry() -> Migration {
    Migration {
        key: KEY,
        version: "0.33.1",
        min_compatible_host_version: Some("0.33.1"),
        rebuilds_referenced_tables: false,
        validate_source: v0_33_0::message_cards_entry().validate,
        apply,
        validate,
    }
}

pub(super) fn prepare_with_progress(
    connection: Connection,
    home: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<(Connection, f02_history::Prepared)> {
    let mut engine = StorageEngine::for_history_upgrade(home, connection)?;
    engine.allow_legacy_history = false;
    engine.recover_session_deletions()?;
    if !engine.recover_session_history_operations()?.is_empty()
        || !engine.recover_body_appends()?.is_empty()
        || !engine.recover_child_body_appends()?.is_empty()
    {
        return Err(MigrationError::Integrity);
    }

    let connection = engine.connection;
    let source = backup::evidence(&connection)?;
    let total = u64::try_from(connection.query_row(
        "SELECT COUNT(*) FROM sessions",
        [],
        |row| row.get::<_, i64>(0),
    )?)
    .map_err(|_| MigrationError::Integrity)?;
    let mut completed = 0;
    progress(completed, total);
    let mut owners = Vec::new();
    let mut after = String::new();
    loop {
        let batch = connection
            .prepare(
                "SELECT session_id,body_generation,message_count FROM sessions
             WHERE session_id>?1 ORDER BY session_id LIMIT 64",
            )?
            .query_map([&after], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if batch.is_empty() {
            break;
        }
        for (raw, generation, count) in batch {
            let (generation, count) = location(generation, count)?;
            let session_id = SessionId::new(raw.clone()).map_err(|_| MigrationError::Integrity)?;
            let owner = ConversationOwner::MainSession {
                session_id: session_id.clone(),
            };
            let (source_file, keys, mut history) = scan_owner(home, &owner, generation, count)?;
            transfer_keys(&connection, &session_id, &keys, &mut history)?;
            let mut children = connection.prepare(
                "SELECT child_task_id,body_generation,message_count FROM child_tasks WHERE session_id=?1",
            )?.query_map([session_id.as_str()], |row| {
                let raw: String = row.get(0)?;
                Ok((raw, (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)))
            })?.map(|row| {
                let (id, (generation, count)) = row?;
                Ok((id, location(generation, count)?))
            }).collect::<Result<HashMap<_, _>>>()?;
            for child in &mut history.child_tasks {
                let control = children.remove(child.child_task_id.as_str());
                if child.body_unavailable {
                    if control.is_some() {
                        return Err(MigrationError::HistorySource(
                            "unavailable child still has execution control",
                        ));
                    }
                    continue;
                }
                let source = (child.body_generation, child.message_count);
                if control.is_some_and(|control| control != source) {
                    return Err(MigrationError::HistorySource(
                        "child catalogue differs from its control",
                    ));
                }
                let (target, prepared) = prepare_child(
                    home,
                    &session_id,
                    child.child_task_id.clone(),
                    source,
                    control.is_some(),
                )?;
                child.body_generation = target;
                owners.push(prepared);
            }
            // 已接纳但尚未写入父目录的 child 仍有独立的目录控制和文件。
            for (raw, source) in children {
                let id = ChildTaskId::new(raw).map_err(|_| MigrationError::Integrity)?;
                let (_, prepared) = prepare_child(home, &session_id, id, source, true)?;
                owners.push(prepared);
            }
            let target = f02_history::next_generation(home, &owner, generation)?;
            owners.push(f02_history::prepare_generation(
                home,
                owner,
                (generation, source_file),
                target,
                count,
                &history,
                conversation::HISTORY_VERSION,
            )?);
            after = raw;
            completed += 1;
            progress(completed, total);
        }
    }
    if completed != total {
        return Err(MigrationError::DatabaseChanged);
    }
    Ok((
        connection,
        f02_history::Prepared::for_input_upgrade(source, owners),
    ))
}

fn location(generation: i64, count: i64) -> Result<(u64, u64)> {
    let generation = u64::try_from(generation)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(MigrationError::Integrity)?;
    Ok((
        generation,
        u64::try_from(count).map_err(|_| MigrationError::Integrity)?,
    ))
}

fn scan_owner(
    home: &Path,
    owner: &ConversationOwner,
    generation: u64,
    count: u64,
) -> Result<(Entry, HistoryMessageKeys, StoredHistoryFacts)> {
    let path = f02_history::owner_path(home, owner, generation)?;
    f02_history::check_managed_ancestors(home, &path)?;
    let file = files::inspect(&path).map_err(f02_history::file_error)?;
    if !matches!(file, Entry::File { .. }) {
        return Err(MigrationError::InvalidPath);
    }
    let (keys, history) = conversation::scan_version(
        &path,
        owner,
        generation,
        conversation::PREVIOUS_HISTORY_VERSION,
    )?;
    if u64::try_from(keys.messages.len()).ok() != Some(count) {
        return Err(MigrationError::HistorySource(
            "message count differs from the catalogue",
        ));
    }
    Ok((
        file,
        keys,
        history.ok_or(MigrationError::HistorySource("format header is missing"))?,
    ))
}

fn prepare_child(
    home: &Path,
    session_id: &SessionId,
    child_task_id: ChildTaskId,
    (generation, count): (u64, u64),
    has_control: bool,
) -> Result<(u64, f02_history::PreparedOwner)> {
    let owner = ConversationOwner::ChildTask {
        session_id: session_id.clone(),
        child_task_id: child_task_id.clone(),
    };
    let (source_file, _, mut history) = scan_owner(home, &owner, generation, count)?;
    if !history.inputs.is_empty() || !history.commands.is_empty() {
        return Err(MigrationError::HistorySource(
            "child body contains session Input or command",
        ));
    }
    let target = f02_history::next_generation(home, &owner, generation)?;
    for child in &mut history.child_tasks {
        if child.child_task_id != child_task_id || child.body_generation != generation {
            return Err(MigrationError::HistorySource(
                "child body refers to a different child or generation",
            ));
        }
        child.body_generation = target;
    }
    let prepared = f02_history::prepare_generation(
        home,
        owner,
        (generation, source_file),
        target,
        count,
        &history,
        conversation::HISTORY_VERSION,
    )?;
    Ok((target, prepared.with_child_control(has_control)))
}

fn transfer_keys(
    connection: &Connection,
    session: &SessionId,
    messages: &HistoryMessageKeys,
    history: &mut StoredHistoryFacts,
) -> Result<()> {
    let mut rows = connection.prepare(
        "SELECT input_id,input_kind,idempotency_key,queue_order,user_message_id
         FROM inputs WHERE session_id=?1 AND state='committed'",
    )?;
    let mut identities = rows
        .query_map([session.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ),
            ))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    for input in &mut history.inputs {
        if !messages.is_user(&input.user_message_id) || input.idempotency_key.is_some() {
            return Err(MigrationError::HistorySource(
                "invalid local Input message or request key",
            ));
        }
        if input.inherited {
            continue;
        }
        let (kind, key, order, message) =
            identities
                .remove(input.input_id.as_str())
                .ok_or(MigrationError::HistorySource(
                    "local Input has no matching SQL identity",
                ))?;
        if kind != "message"
            || u64::try_from(order).ok() != Some(input.queue_order)
            || message.is_some_and(|message| message != input.user_message_id.as_str())
        {
            return Err(MigrationError::HistorySource(
                "local Input differs from its SQL identity",
            ));
        }
        input.idempotency_key = key
            .map(IdempotencyKey::new)
            .transpose()
            .map_err(|_| MigrationError::Integrity)?;
    }
    let inherited: std::collections::HashSet<_> = history
        .message_origins
        .iter()
        .filter(|origin| origin.inherited)
        .map(|origin| &origin.message_id)
        .collect();
    for command in &history.commands {
        if !messages.is_user(&command.user_message_id)
            || !command
                .result
                .as_ref()
                .is_some_and(|result| result.matches_command(&command.command))
        {
            return Err(MigrationError::HistorySource(
                "command message or result is invalid",
            ));
        }
        if inherited.contains(&command.user_message_id) {
            if command.idempotency_key.is_some() {
                return Err(MigrationError::HistorySource(
                    "inherited command has a request key",
                ));
            }
            continue;
        }
        let (kind, key, _, _) =
            identities
                .remove(command.input_id.as_str())
                .ok_or(MigrationError::HistorySource(
                    "local command has no matching SQL identity",
                ))?;
        if kind != "command"
            || key.as_deref() != command.idempotency_key.as_ref().map(IdempotencyKey::as_str)
        {
            return Err(MigrationError::HistorySource(
                "command request key differs from its SQL identity",
            ));
        }
    }
    if !identities.is_empty() {
        return Err(MigrationError::HistorySource(
            "SQL Input identity has no matching history",
        ));
    }
    Ok(())
}

fn apply(connection: &Connection) -> Result<()> {
    // validate_source 保证选择只属于 queued。已完成行不能级联释放任何 Run 或选择。
    connection.execute_batch(
        "UPDATE inputs SET idempotency_key=NULL WHERE state='committed';
         DELETE FROM inputs WHERE state='committed'
          AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.input_id=inputs.input_id)
          AND NOT EXISTS(SELECT 1 FROM skill_activations a WHERE a.input_id=inputs.input_id)
          AND NOT EXISTS(SELECT 1 FROM mcp_input_selections m WHERE m.input_id=inputs.input_id);",
    )?;
    Ok(())
}

fn validate(connection: &Connection) -> Result<()> {
    (v0_33_0::message_cards_entry().validate)(connection)?;
    if connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM inputs i WHERE i.state='committed'
         AND (i.input_kind!='message' OR i.idempotency_key IS NOT NULL
             OR NOT EXISTS(SELECT 1 FROM runs r WHERE r.input_id=i.input_id AND r.session_id=i.session_id)))",
        [], |row| row.get::<_, bool>(0),
    )? {
        return Err(MigrationError::Integrity);
    }
    Ok(())
}
