//! 用户 SQLite 中的计划定义。所有操作由既有 Store worker 串行执行。

use assistant_protocol::{ContextPage, ScheduleId, ScheduleSettings};
use assistant_runtime::{StoredSchedule, StoredSchedulePage};
use rusqlite::{OptionalExtension, params};

use super::{
    StorageEngine, StorageResult, database_write_error, internal_error, invalid_data_with_source,
};

pub(super) mod delivery;
mod executions;

// 只查询当前一次性规则，由已经存在的执行事实推导，无需另存状态。
const ONCE_TRIGGERED: &str =
    "CASE WHEN json_extract(s.settings_json,'$.rule.type')='once' THEN EXISTS(
    SELECT 1 FROM schedule_executions e WHERE e.schedule_id=s.schedule_id
    AND e.scheduled_for_ms IS NOT NULL
    AND json_extract(e.settings_json,'$.rule')=json_extract(s.settings_json,'$.rule')
) ELSE 0 END";

impl StorageEngine {
    pub(super) fn list_schedules(
        &self,
        offset: u32,
        limit: u32,
    ) -> StorageResult<StoredSchedulePage> {
        // 唯一连接上的同一个 worker 命令内读取总数与当前页，其他 Store 操作不会穿插。
        let total = self
            .connection
            .query_row("SELECT COUNT(*) FROM schedules", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|source| internal_error("schedule count could not be read", source))?;
        let sql = format!(
            "SELECT schedule_id, settings_json, next_due_at_ms, deleting, {ONCE_TRIGGERED}
                  FROM schedules s ORDER BY schedule_id LIMIT ?1 OFFSET ?2"
        );
        let mut statement = self
            .connection
            .prepare(&sql)
            .map_err(|source| internal_error("schedules could not be listed", source))?;
        let rows = statement
            .query_map(params![limit, offset], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .map_err(|source| internal_error("schedules could not be listed", source))?;
        let schedules = rows
            .map(|row| {
                let (raw_id, settings, next_due_at_ms, deleting, once_triggered) =
                    row.map_err(|source| invalid_data_with_source("invalid schedule row", source))?;
                let id = ScheduleId::new(raw_id)
                    .map_err(|source| invalid_data_with_source("invalid schedule id", source))?;
                let mut schedule = decode(id, &settings, next_due_at_ms, deleting)?;
                schedule.once_triggered = once_triggered;
                Ok(schedule)
            })
            .collect::<StorageResult<_>>()?;
        Ok(StoredSchedulePage {
            schedules,
            page: ContextPage {
                offset,
                limit,
                total: u32::try_from(total).unwrap_or(u32::MAX),
            },
        })
    }

    pub(super) fn get_schedule(&self, id: ScheduleId) -> StorageResult<Option<StoredSchedule>> {
        let sql = format!(
            "SELECT settings_json, next_due_at_ms, deleting, {ONCE_TRIGGERED}
                  FROM schedules s WHERE schedule_id=?1"
        );
        self.connection
            .query_row(&sql, [id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                ))
            })
            .optional()
            .map_err(|source| internal_error("schedule could not be read", source))?
            .map(|(settings, next_due_at_ms, deleting, once_triggered)| {
                let mut schedule = decode(id, &settings, next_due_at_ms, deleting)?;
                schedule.once_triggered = once_triggered;
                Ok(schedule)
            })
            .transpose()
    }

    pub(super) fn insert_schedule(&self, schedule: StoredSchedule) -> StorageResult<bool> {
        let settings = encode(&schedule.settings)?;
        let changed = self
            .connection
            .execute(
                "INSERT INTO schedules(schedule_id, settings_json, next_due_at_ms) VALUES(?1, ?2, ?3)
             ON CONFLICT(schedule_id) DO NOTHING",
                params![schedule.schedule_id.as_str(), settings, schedule.next_due_at_ms],
            )
            .map_err(|source| database_write_error("schedule could not be created", source))?;
        Ok(changed == 1)
    }

    pub(super) fn replace_schedule(&self, schedule: StoredSchedule) -> StorageResult<bool> {
        let settings = encode(&schedule.settings)?;
        let changed = self
            .connection
            .execute(
                "UPDATE schedules SET settings_json=?2, next_due_at_ms=?3 WHERE schedule_id=?1 AND deleting=0",
                params![
                    schedule.schedule_id.as_str(),
                    settings,
                    schedule.next_due_at_ms
                ],
            )
            .map_err(|source| database_write_error("schedule could not be replaced", source))?;
        Ok(changed == 1)
    }

    pub(super) fn delete_schedule(&mut self, id: ScheduleId) -> StorageResult<bool> {
        if !self.clean_deleting_schedule(&id)? {
            return Ok(false);
        }
        let changed = self
            .connection
            .execute(
                "DELETE FROM schedules WHERE schedule_id=?1 AND deleting=1",
                [id.as_str()],
            )
            .map_err(|source| database_write_error("schedule could not be deleted", source))?;
        Ok(changed == 1)
    }
}

fn encode(settings: &ScheduleSettings) -> StorageResult<String> {
    serde_json::to_string(settings).map_err(|source| {
        invalid_data_with_source("schedule settings could not be encoded", source)
    })
}

fn decode(
    schedule_id: ScheduleId,
    settings: &str,
    next_due_at_ms: Option<i64>,
    deleting: bool,
) -> StorageResult<StoredSchedule> {
    let settings = serde_json::from_str(settings)
        .map_err(|source| invalid_data_with_source("invalid stored schedule settings", source))?;
    Ok(StoredSchedule {
        schedule_id,
        deleting,
        once_triggered: false,
        settings,
        next_due_at_ms,
    })
}
