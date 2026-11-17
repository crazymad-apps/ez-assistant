//! 每个侧栏写入在 worker 独占连接的立即事务内验证与提交。
use super::{
    StorageEngine, StorageResult, database_write_error,
    sidebar_order::{Placement, Scope},
};
use assistant_protocol::{
    MoveSidebarItemRequest, ResetSidebarOrderRequest, SessionGroup, SidebarPlacement,
};
use rusqlite::TransactionBehavior;

fn session_scope(group: &SessionGroup) -> Scope<'_> {
    match group {
        SessionGroup::Workspace { workspace_id } => Scope::Sessions(Some(workspace_id.as_str())),
        SessionGroup::Unbound => Scope::Sessions(None),
    }
}

impl StorageEngine {
    pub(super) fn move_sidebar_item(
        &mut self,
        request: MoveSidebarItemRequest,
    ) -> StorageResult<()> {
        let (scope, item, reference, placement) = match &request {
            MoveSidebarItemRequest::Workspaces {
                workspace_id,
                reference_id,
                placement,
            } => (
                Scope::Workspaces,
                workspace_id.as_str(),
                reference_id.as_str(),
                placement,
            ),
            MoveSidebarItemRequest::Sessions {
                group,
                session_id,
                reference_id,
                placement,
            } => (
                session_scope(group),
                session_id.as_str(),
                reference_id.as_str(),
                placement,
            ),
            MoveSidebarItemRequest::Pins {
                session_id,
                reference_id,
                placement,
            } => (
                Scope::Pins,
                session_id.as_str(),
                reference_id.as_str(),
                placement,
            ),
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| database_write_error("sidebar move could not begin", e))?;
        scope.move_item(
            &transaction,
            item,
            reference,
            match placement {
                SidebarPlacement::Before => Placement::Before,
                SidebarPlacement::After => Placement::After,
            },
        )?;
        transaction
            .commit()
            .map_err(|e| database_write_error("sidebar move could not commit", e))
    }

    pub(super) fn reset_sidebar_order(
        &mut self,
        request: ResetSidebarOrderRequest,
    ) -> StorageResult<()> {
        let scope = match &request {
            ResetSidebarOrderRequest::Workspaces => Scope::Workspaces,
            ResetSidebarOrderRequest::Sessions { group } => session_scope(group),
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| database_write_error("sidebar reset could not begin", e))?;
        scope.reset(&transaction)?;
        transaction
            .commit()
            .map_err(|e| database_write_error("sidebar reset could not commit", e))
    }
}
