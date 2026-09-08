//! 标题检索直接查询已有元数据，与正文索引和已加载会话无关。
use super::{StorageEngine, StorageResult, internal_error, invalid_data};
use assistant_protocol::{
    ChildTaskId, ConversationHistoryHit, ConversationHistoryMatchKind, ConversationOwner,
    SessionId, SessionLifecycle,
};
use assistant_runtime::{ConversationSearchRequest, ConversationSearchScope};
use rusqlite::params;

impl StorageEngine {
    pub(super) fn search_conversation_titles(
        &self,
        request: ConversationSearchRequest,
    ) -> StorageResult<Vec<ConversationHistoryHit>> {
        let (session, workspace) = match &request.scope {
            ConversationSearchScope::Global => (None, None),
            ConversationSearchScope::Session { session_id } => (Some(session_id.as_str()), None),
            ConversationSearchScope::Workspace { workspace_id } => {
                (None, Some(workspace_id.as_str()))
            }
        };
        let mut statement = self.connection.prepare(
            "SELECT s.session_id, s.title, s.lifecycle, NULL AS child_id, NULL AS child_title, s.updated_at_ms AS activity
             FROM sessions s LEFT JOIN session_resources r ON r.session_id = s.session_id
             WHERE (?1 IS NULL OR s.session_id = ?1) AND (?2 IS NULL OR r.workspace_id = ?2)
               AND instr(lower(s.title), lower(?3)) > 0
             UNION ALL
             SELECT s.session_id, s.title, s.lifecycle, c.child_task_id, c.title, COALESCE(c.finished_at_ms, c.created_at_ms)
             FROM child_tasks c JOIN sessions s ON s.session_id = c.session_id
             LEFT JOIN session_resources r ON r.session_id = s.session_id
             WHERE (?1 IS NULL OR s.session_id = ?1) AND (?2 IS NULL OR r.workspace_id = ?2)
               AND instr(lower(c.title), lower(?3)) > 0
             ORDER BY activity DESC LIMIT ?4"
        ).map_err(|e| internal_error("history titles could not be queried", e))?;
        let rows = statement
            .query_map(
                params![
                    session,
                    workspace,
                    request.query,
                    request.limit.min(200) as i64
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .map_err(|e| internal_error("history titles could not be read", e))?;
        rows.map(|row| {
            let (id, session_title, lifecycle, child_id, child_task_title, activity) =
                row.map_err(|e| internal_error("history title row is invalid", e))?;
            let session_id =
                SessionId::new(id).map_err(|_| invalid_data("invalid history session id"))?;
            let owner = match child_id {
                Some(id) => ConversationOwner::ChildTask {
                    session_id,
                    child_task_id: ChildTaskId::new(id)
                        .map_err(|_| invalid_data("invalid history child id"))?,
                },
                None => ConversationOwner::MainSession { session_id },
            };
            Ok(ConversationHistoryHit {
                owner,
                snippet: child_task_title.as_ref().unwrap_or(&session_title).clone(),
                session_title,
                child_task_title,
                lifecycle: match lifecycle.as_str() {
                    "active" => SessionLifecycle::Active,
                    "archived" => SessionLifecycle::Archived,
                    _ => return Err(invalid_data("invalid history lifecycle")),
                },
                message_id: None,
                created_at_ms: Some(activity),
                match_kind: ConversationHistoryMatchKind::Title,
            })
        })
        .collect()
    }
}
