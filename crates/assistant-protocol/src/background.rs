//! 后台 Shell 的会话级命令结果与观察投影。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{RunId, SessionId};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum BackgroundRunStatus {
    Running,
    CleanupUnconfirmed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct BackgroundRunSummary {
    pub background_id: String,
    pub run_id: RunId,
    pub command_summary: String,
    /// 仅在按后台标识查询且显式请求时返回。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub command: Option<String>,
    pub working_directory: String,
    pub started_at_ms: i64,
    pub status: BackgroundRunStatus,
    pub log_path: Option<String>,
    pub log_available: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListBackgroundRunsRequest {
    pub session_id: SessionId,
    #[serde(default)]
    #[ts(optional)]
    pub offset: Option<u32>,
    #[serde(default)]
    #[ts(optional)]
    pub limit: Option<u32>,
    #[serde(default)]
    #[ts(optional)]
    pub status: Option<BackgroundRunStatus>,
    #[serde(default)]
    #[ts(optional)]
    pub background_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub include_command: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListBackgroundRunsResult {
    pub runs: Vec<BackgroundRunSummary>,
    pub offset: u32,
    pub limit: u32,
    pub total: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReadBackgroundOutputRequest {
    pub session_id: SessionId,
    pub background_id: String,
    #[serde(default)]
    #[ts(optional)]
    pub cursor: Option<u64>,
    #[serde(default)]
    #[ts(optional)]
    pub offset_chars: Option<i64>,
    #[serde(default)]
    #[ts(optional)]
    pub limit_chars: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReadBackgroundOutputResult {
    pub status: BackgroundRunStatus,
    pub content: String,
    pub cursor: u64,
    pub next_cursor: u64,
    pub dropped_chars: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct StopBackgroundRunRequest {
    pub session_id: SessionId,
    pub background_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct StopBackgroundRunResult {
    pub background_id: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// 进程已回收。输出或文件清理错误仍需要向用户反馈。
    pub output_error: Option<String>,
}
