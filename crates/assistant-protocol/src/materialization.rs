//! 新会话首次发送的批量物化传输契约。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    AgentVariant, ApprovalMode, AttachmentSummary, IdempotencyKey, InputId, ModelKey,
    QuotedTextSnapshot, ReasoningEffortKey, RunSnapshot, SessionSummary, SubmitInputMode,
    WorkspaceId,
};

/// multipart 中一个文件字段的声明；字段名必须等于 `selection_key`。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionMaterializationAttachment {
    pub selection_key: String,
    pub original_name: String,
    pub size_bytes: u64,
}

/// 新会话首次发送的完整业务意图。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionMaterializationManifest {
    pub idempotency_key: IdempotencyKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model_key: Option<ModelKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reasoning_effort: Option<ReasoningEffortKey>,
    pub variant: AgentVariant,
    pub approval_mode: ApprovalMode,
    pub message: String,
    #[serde(default)]
    pub mode: SubmitInputMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<SessionMaterializationAttachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quotes: Vec<QuotedTextSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub skill_name: Option<String>,
}

/// 首次发送可靠提交后返回给 Desktop 的完整定位结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionMaterializationResult {
    pub session: SessionSummary,
    pub input_id: InputId,
    pub run: RunSnapshot,
    pub attachments: Vec<AttachmentSummary>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_defaults_optional_input_parts() {
        let json = r#"{
            "idempotency_key":"materialize-1",
            "variant":"build",
            "approval_mode":"ask",
            "message":"hello"
        }"#;
        let manifest = serde_json::from_str::<SessionMaterializationManifest>(json)
            .expect("manifest should decode");
        assert_eq!(manifest.mode, SubmitInputMode::Normal);
        assert!(manifest.attachments.is_empty());
        assert!(manifest.quotes.is_empty());
        assert!(manifest.workspace_id.is_none());
    }
}
