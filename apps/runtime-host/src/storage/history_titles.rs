//! 标题来自会话元数据、文件 child 目录与真实未完成控制，不使用派生文本索引发现历史。

use std::collections::BTreeMap;

use assistant_protocol::{
    ChildTaskId, ConversationHistoryHit, ConversationHistoryMatchKind, ConversationOwner,
    SessionId, SessionLifecycle,
};
use assistant_runtime::{
    ConversationSearchPage, ConversationSearchRequest, ConversationSearchScope,
};
use rusqlite::params;

use super::{StorageEngine, StorageResult, internal_error, invalid_data};

impl StorageEngine {
    /// 单 Store worker 内只读捕获目录，不修复文件或装配执行。坏 owner 保留诊断，其他标题仍可返回；
    /// 输入先按 Session／Workspace 过滤，每次仅保留结果上限内的标题，不另存历史 child 索引。
    pub(super) fn search_conversation_titles(
        &mut self,
        request: ConversationSearchRequest,
    ) -> StorageResult<ConversationSearchPage<ConversationHistoryHit>> {
        let limit = request.limit.min(200);
        let mut page = ConversationSearchPage {
            hits: Vec::with_capacity(limit),
            partial: false,
            failed_owners: Vec::new(),
        };
        if limit == 0 {
            return Ok(page);
        }
        let (session, workspace) = match &request.scope {
            ConversationSearchScope::Global => (None, None),
            ConversationSearchScope::Session { session_id } => (Some(session_id.as_str()), None),
            ConversationSearchScope::Workspace { workspace_id } => {
                (None, Some(workspace_id.as_str()))
            }
        };
        let sessions = self
            .connection
            .prepare(
                "SELECT s.session_id,s.title,s.lifecycle,s.updated_at_ms FROM sessions s
             LEFT JOIN session_resources r ON r.session_id=s.session_id
             WHERE (?1 IS NULL OR s.session_id=?1) AND (?2 IS NULL OR r.workspace_id=?2)
             ORDER BY s.updated_at_ms DESC,s.session_id",
            )
            .map_err(|source| {
                internal_error("history title sessions could not be queried", source)
            })?
            .query_map(params![session, workspace], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|source| internal_error("history title sessions could not be read", source))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source| internal_error("history title session row is invalid", source))?;
        let query = request.query.to_lowercase();
        for (id, title, lifecycle, activity) in sessions {
            let session_id =
                SessionId::new(id).map_err(|_| invalid_data("invalid history session id"))?;
            let lifecycle = match lifecycle.as_str() {
                "active" => SessionLifecycle::Active,
                "archived" => SessionLifecycle::Archived,
                _ => return Err(invalid_data("invalid history lifecycle")),
            };
            if title.to_lowercase().contains(&query) {
                retain_hit(
                    &mut page.hits,
                    ConversationHistoryHit {
                        owner: ConversationOwner::MainSession {
                            session_id: session_id.clone(),
                        },
                        session_title: title.clone(),
                        child_task_title: None,
                        message_id: None,
                        created_at_ms: Some(activity),
                        snippet: title.clone(),
                        match_kind: ConversationHistoryMatchKind::Title,
                        lifecycle,
                    },
                    limit,
                );
            }
            let mut children = BTreeMap::new();
            match self.load_child_history_catalog(&session_id) {
                Ok(facts) => {
                    for child in facts {
                        children.insert(
                            child.child_task_id,
                            (
                                child.title,
                                child.finished_at_ms.unwrap_or(child.created_at_ms),
                            ),
                        );
                    }
                }
                Err(_) => {
                    page.partial = true;
                    page.failed_owners.push(ConversationOwner::MainSession {
                        session_id: session_id.clone(),
                    });
                }
            }
            // 父交换未可靠闭合时，文件目录可能尚无该 child；只补充真实未完成控制的标题。
            let mut statement = self.connection.prepare(
                "SELECT child_task_id,title,created_at_ms FROM child_tasks WHERE session_id=?1 AND status IN ('accepted','running') ORDER BY child_task_id",
            ).map_err(|source| internal_error("history title controls could not be queried",source))?;
            let rows = statement
                .query_map([session_id.as_str()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .map_err(|source| {
                    internal_error("history title controls could not be read", source)
                })?;
            for row in rows {
                let (id, title, activity) = row.map_err(|source| {
                    internal_error("history title control row is invalid", source)
                })?;
                let child =
                    ChildTaskId::new(id).map_err(|_| invalid_data("invalid history child id"))?;
                children.insert(child, (title, activity));
            }
            for (child, (child_title, activity)) in children {
                if child_title.to_lowercase().contains(&query) {
                    retain_hit(
                        &mut page.hits,
                        ConversationHistoryHit {
                            owner: ConversationOwner::ChildTask {
                                session_id: session_id.clone(),
                                child_task_id: child,
                            },
                            session_title: title.clone(),
                            child_task_title: Some(child_title.clone()),
                            message_id: None,
                            created_at_ms: Some(activity),
                            snippet: child_title,
                            match_kind: ConversationHistoryMatchKind::Title,
                            lifecycle,
                        },
                        limit,
                    );
                }
            }
        }
        Ok(page)
    }
}

fn retain_hit(hits: &mut Vec<ConversationHistoryHit>, hit: ConversationHistoryHit, limit: usize) {
    hits.push(hit);
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.created_at_ms));
    hits.truncate(limit);
}
