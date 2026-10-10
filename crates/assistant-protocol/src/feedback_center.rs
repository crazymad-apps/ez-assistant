//! 反馈中心客户端契约；不持有身份凭据、持久化或 Agent 问答状态。
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum FeedbackTargetKind {
    Platform,
    Plugin,
    Knowledge,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(deny_unknown_fields)]
pub struct FeedbackTarget {
    pub kind: FeedbackTargetKind,
    pub reference: Option<String>,
    pub name: String,
    pub version: Option<String>,
    pub content_ref: Option<String>,
    pub location: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(deny_unknown_fields)]
pub struct FeedbackContent {
    pub target: FeedbackTarget,
    pub title: String,
    pub description: String,
    pub reproduction: Option<String>,
    pub expected_result: Option<String>,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum FeedbackClientKind {
    Desktop,
    Web,
    Agent,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(deny_unknown_fields)]
pub struct FeedbackClientEnvironment {
    pub client_kind: FeedbackClientKind,
    pub client_version: Option<String>,
    pub client_os: Option<String>,
    pub client_arch: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct FeedbackEnvironment {
    pub host_version: Option<String>,
    pub host_os: Option<String>,
    pub host_arch: Option<String>,
    #[serde(flatten)]
    #[ts(flatten)]
    pub client: FeedbackClientEnvironment,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(deny_unknown_fields)]
pub struct SubmitFeedbackRequest {
    pub submission_id: String,
    pub content: FeedbackContent,
    pub client: FeedbackClientEnvironment,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SubmittedFeedbackStatus {
    Pending,
    Processing,
    Resolved,
    Closed,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum FeedbackSource {
    Manual,
    Agent,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SubmittedFeedbackSummary {
    pub id: String,
    pub title: String,
    pub target_kind: FeedbackTargetKind,
    pub target_name: String,
    pub status: SubmittedFeedbackStatus,
    pub source: FeedbackSource,
    pub owner_user_id: i32,
    pub owner_username: String,
    pub owner_display_name: String,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SubmittedFeedback {
    #[serde(flatten)]
    #[ts(flatten)]
    pub summary: SubmittedFeedbackSummary,
    pub submission_id: String,
    pub target: FeedbackTarget,
    pub description: String,
    pub reproduction: Option<String>,
    pub expected_result: Option<String>,
    pub environment: FeedbackEnvironment,
    pub handling_note: Option<String>,
    pub handler_user_id: Option<i32>,
    pub handler_username: Option<String>,
    pub handler_display_name: Option<String>,
}
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(deny_unknown_fields)]
pub struct ListSubmittedFeedbackRequest {
    pub offset: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<SubmittedFeedbackStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub target_kind: Option<FeedbackTargetKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub submission_id: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(deny_unknown_fields)]
pub struct GetSubmittedFeedbackRequest {
    pub id: String,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SubmittedFeedbackPage {
    pub items: Vec<SubmittedFeedbackSummary>,
    pub total: u32,
    pub limit: u32,
    pub offset: u32,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct FeedbackSupport {
    pub available: bool,
    pub host_version: String,
    pub host_os: String,
    pub host_arch: String,
}
/// 独立业务错误；普通参数或中心读取失败不应注销 Host 登录。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum FeedbackFailure {
    Unsupported,
    InvalidInput,
    TooLarge,
    NotFound,
    Conflict,
    Unavailable,
    IdentityChanged,
    /// POST 已开始但未收到可靠回执；不可解释为未写入，也不自动重放。
    SubmissionUnknown,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum FeedbackResponse<T> {
    Success(T),
    Failure(FeedbackFailure),
}
impl<T> From<Result<T, FeedbackFailure>> for FeedbackResponse<T> {
    fn from(value: Result<T, FeedbackFailure>) -> Self {
        match value {
            Ok(value) => Self::Success(value),
            Err(error) => Self::Failure(error),
        }
    }
}
