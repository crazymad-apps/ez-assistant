//! 侧栏排序意图；位置仅由相邻实体表达，不暴露存储排序值。
use crate::{SessionGroup, SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum SidebarPlacement {
    Before,
    After,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub enum MoveSidebarItemRequest {
    Workspaces {
        workspace_id: WorkspaceId,
        reference_id: WorkspaceId,
        placement: SidebarPlacement,
    },
    Sessions {
        group: SessionGroup,
        session_id: SessionId,
        reference_id: SessionId,
        placement: SidebarPlacement,
    },
    Pins {
        session_id: SessionId,
        reference_id: SessionId,
        placement: SidebarPlacement,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ResetSidebarOrderRequest {
    Workspaces,
    Sessions { group: SessionGroup },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SidebarOrderResult {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RuntimeCommand, RuntimeCommandResult, RuntimeEvent};
    use serde_json::json;

    #[test]
    fn sidebar_wire_contract_uses_adjacent_ids_and_rejects_pin_reset() {
        let command = json!({"type":"move_sidebar_item","payload":{"scope":"sessions","group":{"type":"unbound"},"session_id":"a","reference_id":"b","placement":"before"}});
        let parsed: RuntimeCommand = serde_json::from_value(command.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), command);
        assert!(
            serde_json::from_value::<ResetSidebarOrderRequest>(json!({"scope":"pins"})).is_err()
        );
        assert!(serde_json::from_value::<MoveSidebarItemRequest>(json!({"scope":"pins","session_id":"a","reference_id":"b","placement":"before","sort_order":12})).is_err());
        assert_eq!(
            serde_json::to_value(RuntimeCommandResult::MoveSidebarItem(SidebarOrderResult {}))
                .unwrap(),
            json!({"type":"move_sidebar_item","payload":{}})
        );
        assert_eq!(
            serde_json::to_value(RuntimeEvent::SidebarOrderChanged).unwrap(),
            json!({"type":"sidebar_order_changed"})
        );
    }
}
