//! 同一用户域的侧栏顺序。排序值只在 SQLite 内使用，所有写入借用调用者的立即事务。
//! 分组包含隐藏条目，确保归档、移除及取消置顶不丢失原位置。

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{StorageResult, conflict, database_write_error, internal_error, invalid_data};

const GAP: i64 = 1024;

/// 数据库内的排序范围，不代表新的业务实体或协议 DTO。
#[derive(Clone, Copy)]
pub(super) enum Scope<'a> {
    Workspaces,
    Sessions(Option<&'a str>),
    Pins,
}

#[derive(Clone, Copy)]
pub(super) enum Placement {
    Before,
    After,
}

impl<'a> Scope<'a> {
    fn table(self) -> &'static str {
        match self {
            Self::Workspaces => "workspaces",
            Self::Sessions(_) => "session_resources",
            Self::Pins => "sessions",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Workspaces => "workspace_id",
            Self::Sessions(_) | Self::Pins => "session_id",
        }
    }

    fn field(self) -> &'static str {
        match self {
            Self::Workspaces | Self::Sessions(_) => "sort_order",
            Self::Pins => "pin_order",
        }
    }

    fn predicate(self) -> &'static str {
        match self {
            Self::Workspaces => "?1 IS NULL",
            Self::Sessions(_) => "workspace_id IS ?1",
            Self::Pins => "is_pinned = 1 AND ?1 IS NULL",
        }
    }

    fn binding(self) -> Option<&'a str> {
        match self {
            Self::Sessions(workspace) => workspace,
            _ => None,
        }
    }

    /// 由现有创建／置顶事务调用；越界时只重排同一范围，不触碰时间或消息字段。
    pub(super) fn next(
        self,
        transaction: &Transaction<'_>,
        placement: Placement,
    ) -> StorageResult<i64> {
        let front = matches!(placement, Placement::Before);
        let edge = if front { "MAX" } else { "MIN" };
        let sql = format!(
            "SELECT {edge}({}) FROM {} WHERE {}",
            self.field(),
            self.table(),
            self.predicate()
        );
        let value: Option<i64> = transaction
            .query_row(&sql, [self.binding()], |row| row.get(0))
            .map_err(|e| internal_error("sidebar order edge could not be read", e))?;
        let Some(value) = value else {
            return Ok(GAP);
        };
        if let Some(value) = if front {
            value.checked_add(GAP)
        } else {
            value.checked_sub(GAP)
        } {
            return Ok(value);
        }
        let ids = self.ids(transaction)?;
        self.renumber(transaction, &ids)?;
        if front {
            rank(
                ids.len()
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("sidebar order capacity exceeded"))?,
            )
        } else {
            Ok(0)
        }
    }

    fn ids(self, connection: &Connection) -> StorageResult<Vec<String>> {
        let sql = format!(
            "SELECT {} FROM {} WHERE {} ORDER BY {} DESC, {}",
            self.id(),
            self.table(),
            self.predicate(),
            self.field(),
            self.id()
        );
        connection
            .prepare(&sql)
            .and_then(|mut s| s.query_map([self.binding()], |r| r.get(0))?.collect())
            .map_err(|e| internal_error("sidebar order could not be read", e))
    }

    fn renumber(self, transaction: &Transaction<'_>, ids: &[String]) -> StorageResult<()> {
        rank(ids.len())?;
        let sql = format!(
            "UPDATE {} SET {} = ?2 WHERE {} = ?3 AND {}",
            self.table(),
            self.field(),
            self.id(),
            self.predicate()
        );
        let mut statement = transaction
            .prepare(&sql)
            .map_err(|e| internal_error("sidebar reorder could not be prepared", e))?;
        for (index, id) in ids.iter().enumerate() {
            if statement
                .execute(params![self.binding(), rank(ids.len() - index)?, id])
                .map_err(|e| database_write_error("sidebar reorder failed", e))?
                != 1
            {
                return Err(conflict("sidebar item changed during reorder"));
            }
        }
        Ok(())
    }

    fn visible(self) -> &'static str {
        match self {
            Self::Workspaces => "lifecycle = 'active'",
            Self::Sessions(_) => {
                "EXISTS(SELECT 1 FROM sessions s WHERE s.session_id = session_resources.session_id AND s.lifecycle = 'active' AND s.role = 'standard') AND (workspace_id IS NULL OR EXISTS(SELECT 1 FROM workspaces w WHERE w.workspace_id = session_resources.workspace_id AND w.lifecycle = 'active'))"
            }
            Self::Pins => {
                "lifecycle = 'active' AND role = 'standard' AND EXISTS(SELECT 1 FROM session_resources r WHERE r.session_id = sessions.session_id AND (r.workspace_id IS NULL OR EXISTS(SELECT 1 FROM workspaces w WHERE w.workspace_id = r.workspace_id AND w.lifecycle = 'active')))"
            }
        }
    }

    /// A 与参照项必须可见；邻居包含隐藏项。失败由外层事务整体回滚，不发布部分重排。
    pub(super) fn move_item(
        self,
        transaction: &Transaction<'_>,
        item: &str,
        reference: &str,
        placement: Placement,
    ) -> StorageResult<()> {
        if item == reference {
            return Err(conflict("sidebar move requires different items"));
        }
        let sql = format!(
            "SELECT {} FROM {} WHERE {} AND {} = ?2 AND ({})",
            self.field(),
            self.table(),
            self.predicate(),
            self.id(),
            self.visible()
        );
        let mut statement = transaction
            .prepare(&sql)
            .map_err(|e| internal_error("sidebar move could not be prepared", e))?;
        for id in [item, reference] {
            if statement
                .query_row(params![self.binding(), id], |r| r.get::<_, i64>(0))
                .optional()
                .map_err(|e| internal_error("sidebar move item could not be read", e))?
                .is_none()
            {
                return Err(conflict(
                    "sidebar item is no longer available in this scope",
                ));
            }
        }
        let value: i64 = statement
            .query_row(params![self.binding(), reference], |r| r.get(0))
            .map_err(|e| internal_error("sidebar reference could not be read", e))?;
        let (comparison, tie, direction) = match placement {
            Placement::Before => (">", "<", "ASC"),
            Placement::After => ("<", ">", "DESC"),
        };
        let sql = format!(
            "SELECT {field} FROM {table} WHERE {scope} AND {id} != ?2 AND ({field} {comparison} ?3 OR ({field} = ?3 AND {id} {tie} ?4)) ORDER BY {field} {direction}, {id} {} LIMIT 1",
            if matches!(placement, Placement::Before) {
                "DESC"
            } else {
                "ASC"
            },
            field = self.field(),
            table = self.table(),
            scope = self.predicate(),
            id = self.id()
        );
        let neighbor: Option<i64> = transaction
            .query_row(&sql, params![self.binding(), item, value, reference], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| internal_error("sidebar neighbor could not be read", e))?;
        let target = match neighbor {
            Some(other) if (i128::from(other) - i128::from(value)).abs() > 1 => {
                Some(((i128::from(other) + i128::from(value)) / 2) as i64)
            }
            Some(_) => None,
            None => match placement {
                Placement::Before => value.checked_add(GAP),
                Placement::After => value.checked_sub(GAP),
            },
        };
        if let Some(target) = target {
            let sql = format!(
                "UPDATE {} SET {} = ?2 WHERE {} = ?3 AND {}",
                self.table(),
                self.field(),
                self.id(),
                self.predicate()
            );
            if transaction
                .execute(&sql, params![self.binding(), target, item])
                .map_err(|e| database_write_error("sidebar move failed", e))?
                != 1
            {
                return Err(conflict("sidebar item changed during move"));
            }
            return Ok(());
        }
        let mut ids = self.ids(transaction)?;
        ids.retain(|id| id != item);
        let index = ids
            .iter()
            .position(|id| id == reference)
            .ok_or_else(|| conflict("sidebar reference changed"))?;
        ids.insert(
            index + usize::from(matches!(placement, Placement::After)),
            item.to_owned(),
        );
        self.renumber(transaction, &ids)
    }

    /// 默认顺序涵盖隐藏条目；单条窗口更新只写本范围的排序值，置顶区没有恢复默认能力。
    pub(super) fn reset(self, transaction: &Transaction<'_>) -> StorageResult<()> {
        if let Self::Sessions(Some(workspace)) = self {
            let active: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM workspaces WHERE workspace_id = ?1 AND lifecycle = 'active')",
                    [workspace],
                    |row| row.get(0),
                )
                .map_err(|e| internal_error("sidebar reset workspace could not be read", e))?;
            if !active {
                return Err(conflict("sidebar workspace is no longer available"));
            }
        }
        let (source, created) = match self {
            Self::Workspaces => ("workspaces", "created_at_ms"),
            Self::Sessions(_) => (
                "session_resources JOIN sessions USING(session_id)",
                "sessions.created_at_ms",
            ),
            Self::Pins => return Err(conflict("pinned order has no default reset")),
        };
        let sql = format!(
            "WITH ranked AS MATERIALIZED (SELECT {id}, ROW_NUMBER() OVER (ORDER BY {created}, {id} DESC) * 1024 AS value FROM {source} WHERE {scope}) UPDATE {table} SET {field} = ranked.value FROM ranked WHERE {table}.{id} = ranked.{id}",
            id = self.id(),
            scope = self.predicate(),
            table = self.table(),
            field = self.field()
        );
        transaction
            .execute(&sql, [self.binding()])
            .map_err(|e| database_write_error("sidebar default order could not be restored", e))?;
        Ok(())
    }

    /// M1 在同一存储调用内将这些有序 ID 投影为列表摘要；取页先过滤生命周期与角色。
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "M1 接入分组分页，M0 验证实际索引查询")
    )]
    pub(super) fn page(
        self,
        connection: &Connection,
        archived: bool,
        offset: u32,
        limit: Option<u32>,
    ) -> StorageResult<Vec<String>> {
        let lifecycle = if archived { "archived" } else { "active" };
        let sql = match self {
            Self::Workspaces => "SELECT workspace_id FROM workspaces WHERE lifecycle = ?2 AND ?1 IS NULL ORDER BY sort_order DESC, workspace_id LIMIT ?3 OFFSET ?4".into(),
            Self::Sessions(_) => "SELECT r.session_id FROM session_resources r CROSS JOIN sessions s ON s.session_id = r.session_id WHERE r.workspace_id IS ?1 AND s.lifecycle = ?2 AND s.role = 'standard' ORDER BY r.sort_order DESC, r.session_id LIMIT ?3 OFFSET ?4".into(),
            Self::Pins => format!("SELECT session_id FROM sessions WHERE is_pinned = 1 AND ?1 IS NULL AND ?2 = 'active' AND ({}) ORDER BY pin_order DESC, session_id LIMIT ?3 OFFSET ?4", self.visible()),
        };
        let lifecycle = if matches!(self, Self::Workspaces) && archived {
            "removed"
        } else {
            lifecycle
        };
        let limit = if matches!(self, Self::Pins) {
            2048
        } else {
            limit.map(i64::from).unwrap_or(-1)
        };
        connection
            .prepare(&sql)
            .and_then(|mut s| {
                s.query_map(params![self.binding(), lifecycle, limit, offset], |r| {
                    r.get(0)
                })?
                .collect()
            })
            .map_err(|e| internal_error("sidebar page could not be read", e))
    }
}

fn rank(count: usize) -> StorageResult<i64> {
    i64::try_from(count)
        .ok()
        .and_then(|count| count.checked_mul(GAP))
        .ok_or_else(|| invalid_data("sidebar order capacity exceeded"))
}

#[cfg(test)]
mod tests;
