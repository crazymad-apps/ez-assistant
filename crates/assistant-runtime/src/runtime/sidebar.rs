//! 侧栏写入只经过 operation gate 与 Store，不装配 Session 或获取会话执行锁。
use super::AssistantRuntime;
use crate::{RuntimeError, RuntimeResult};
use assistant_protocol::{
    MoveSidebarItemRequest, ResetSidebarOrderRequest, RuntimeEvent, SidebarOrderResult,
};

impl AssistantRuntime {
    pub async fn move_sidebar_item(
        &self,
        request: MoveSidebarItemRequest,
    ) -> RuntimeResult<SidebarOrderResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        self.store
            .move_sidebar_item(request)
            .await
            .map_err(|source| sidebar_error("move sidebar item", source))?;
        self.publish(RuntimeEvent::SidebarOrderChanged);
        Ok(SidebarOrderResult {})
    }

    pub async fn reset_sidebar_order(
        &self,
        request: ResetSidebarOrderRequest,
    ) -> RuntimeResult<SidebarOrderResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        self.store
            .reset_sidebar_order(request)
            .await
            .map_err(|source| sidebar_error("reset sidebar order", source))?;
        self.publish(RuntimeEvent::SidebarOrderChanged);
        Ok(SidebarOrderResult {})
    }
}

fn sidebar_error(operation: &'static str, source: crate::StoreError) -> RuntimeError {
    if source.kind() == crate::StoreErrorKind::Conflict {
        RuntimeError::SidebarOrderConflict
    } else {
        RuntimeError::from_store(operation, source)
    }
}
