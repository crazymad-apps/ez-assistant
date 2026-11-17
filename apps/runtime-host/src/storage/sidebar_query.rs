//! 一次 worker 调用与 SQLite 读事务取得所有列表页，外层 Runtime 核对同一事件水位。
use super::{StorageEngine, StorageResult, internal_error, invalid_data};
use assistant_protocol::{
    GetApplicationSnapshotRequest, ListWorkspacesRequest, SessionGroup, SessionGroupRequest,
    SessionGroupSnapshot, SessionListFilter,
};
use assistant_runtime::{SIDEBAR_PAGE_SIZE, SessionSummaryQuery, SidebarSnapshot};

impl StorageEngine {
    pub(super) fn query_sidebar(
        &self,
        request: GetApplicationSnapshotRequest,
    ) -> StorageResult<SidebarSnapshot> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|e| internal_error("sidebar read transaction could not begin", e))?;
        let workspaces = self.query_workspaces(ListWorkspacesRequest {
            limit: Some(request.workspaces_limit.unwrap_or(SIDEBAR_PAGE_SIZE)),
            ..Default::default()
        })?;
        let requests = request.session_groups.unwrap_or_else(|| {
            workspaces
                .workspaces
                .iter()
                .map(|workspace| SessionGroupRequest {
                    group: SessionGroup::Workspace {
                        workspace_id: workspace.workspace_id.clone(),
                    },
                    filter: SessionListFilter::Active,
                    limit: None,
                })
                .chain(std::iter::once(SessionGroupRequest {
                    group: SessionGroup::Unbound,
                    filter: SessionListFilter::Active,
                    limit: None,
                }))
                .collect()
        });
        let mut session_groups = Vec::with_capacity(requests.len());
        for request in requests {
            if request.filter == SessionListFilter::All {
                return Err(invalid_data("sidebar group requires one lifecycle"));
            }
            let limit = request
                .limit
                .unwrap_or(SIDEBAR_PAGE_SIZE)
                .clamp(1, u32::MAX - 1);
            let mut sessions = self.query_session_summaries(SessionSummaryQuery {
                group: Some(request.group.clone()),
                filter: request.filter,
                limit: limit + 1,
                ..Default::default()
            })?;
            let next_offset = (sessions.len() > limit as usize).then_some(limit);
            sessions.truncate(limit as usize);
            session_groups.push(SessionGroupSnapshot {
                group: request.group,
                filter: request.filter,
                sessions,
                next_offset,
            });
        }
        let pinned_sessions = self.query_session_summaries(SessionSummaryQuery {
            pinned_only: true,
            limit: 2048,
            ..Default::default()
        })?;
        transaction
            .commit()
            .map_err(|e| internal_error("sidebar read transaction could not finish", e))?;
        Ok(SidebarSnapshot {
            workspaces,
            session_groups,
            pinned_sessions,
        })
    }
}
