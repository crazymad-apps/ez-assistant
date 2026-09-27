//! 主会话 ask_user 问答：客户端只提交选择位置和原文，问题与状态由 Runtime 持有。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{RunId, SessionId, ToolCallId};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum FeedbackMode {
    #[default]
    Wait,
    EndTurn,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct FeedbackOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct FeedbackQuestion {
    pub question: String,
    pub header: Option<String>,
    pub options: Vec<FeedbackOption>,
    pub multiple: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct FeedbackAnswer {
    pub selected_options: Vec<u32>,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum FeedbackStatus {
    Pending,
    Answered,
    Cancelled,
    Invalidated,
}

/// 请求身份由 Runtime 生成；答案与模式不能由客户端修改。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct FeedbackRequestSnapshot {
    pub request_id: String,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub tool_call_id: ToolCallId,
    pub mode: FeedbackMode,
    pub questions: Vec<FeedbackQuestion>,
    pub status: FeedbackStatus,
    pub answers: Option<Vec<FeedbackAnswer>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListFeedbackRequestsRequest {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct AnswerFeedbackRequest {
    pub session_id: SessionId,
    pub request_id: String,
    pub answers: Vec<FeedbackAnswer>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CancelFeedbackRequest {
    pub session_id: SessionId,
    pub request_id: String,
}
