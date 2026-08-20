//! Safety Demo 私有的内存审计投影。

use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use agent_tools::{
    FileAuthorizationFacts, GeneralAuthorizationFacts, ResolvedToolInvocation,
    ShellAuthorizationFacts, ShellProcessMode,
};
use agent_types::{ToolCallId, ToolResult, ToolResultStatus};
use serde::{Deserialize, Serialize};

/// 审计中允许展示的 resolved 调用事实，不保存文件内容或 Shell 输出。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AuditFacts {
    General {
        tool_name: String,
    },
    File {
        operation: String,
        path: String,
    },
    Shell {
        command: String,
        workdir: String,
        timeout_ms: u64,
        process_mode: String,
    },
}

impl AuditFacts {
    pub(crate) fn from_invocation(invocation: &ResolvedToolInvocation) -> Self {
        if let Some(facts) = invocation.facts::<FileAuthorizationFacts>() {
            return Self::File {
                operation: format!("{:?}", facts.operation).to_ascii_lowercase(),
                path: facts.path.to_string(),
            };
        }
        if let Some(facts) = invocation.facts::<ShellAuthorizationFacts>() {
            return Self::Shell {
                command: facts.command.clone(),
                workdir: facts.workdir.to_string(),
                timeout_ms: u64::try_from(facts.timeout.as_millis()).unwrap_or(u64::MAX),
                process_mode: match facts.process_mode {
                    ShellProcessMode::Managed => "managed",
                    ShellProcessMode::Detached => "detached",
                }
                .to_owned(),
            };
        }
        let tool_name = invocation.facts::<GeneralAuthorizationFacts>().map_or_else(
            || invocation.tool_name().as_str().to_owned(),
            |facts| facts.tool_name.as_str().to_owned(),
        );
        Self::General { tool_name }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuditDecision {
    Allow,
    Deny,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuditExecutionStatus {
    Authorized,
    WaitingApproval,
    Denied,
    Started,
    Completed,
    Failed,
    Cancelled,
}

/// 单次工具调用的审计记录。序号只在本 Demo 进程内单调递增。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct AuditEntry {
    pub sequence: u64,
    /// Demo 进程记录该事实时的 Unix 毫秒时间戳，仅供页面按时间展示。
    pub timestamp_ms: u64,
    pub run_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub facts: AuditFacts,
    pub policy: String,
    pub rule: String,
    pub decision: Option<AuditDecision>,
    pub approval_id: Option<String>,
    pub status: AuditExecutionStatus,
    pub error_class: Option<String>,
    pub exit_code: Option<i32>,
}

#[derive(Default)]
struct AuditState {
    entries: Vec<AuditEntry>,
    next_sequence: u64,
}

/// 可被同步策略与异步 Runtime 共同使用的短临界区内存审计。
#[derive(Clone, Default)]
pub(crate) struct DemoAudit {
    state: Arc<Mutex<AuditState>>,
}

impl DemoAudit {
    pub(crate) fn entries(&self) -> Vec<AuditEntry> {
        self.lock().entries.clone()
    }

    pub(crate) fn clear(&self) {
        self.lock().entries.clear();
    }

    pub(crate) fn record_policy(
        &self,
        run_id: &str,
        invocation: &ResolvedToolInvocation,
        policy: &str,
        rule: &str,
        decision: AuditDecision,
    ) {
        let status = match decision {
            AuditDecision::Allow => AuditExecutionStatus::Authorized,
            AuditDecision::Deny => AuditExecutionStatus::Denied,
        };
        self.insert(AuditEntry {
            sequence: 0,
            timestamp_ms: 0,
            run_id: run_id.to_owned(),
            call_id: invocation.call_id().to_string(),
            tool_name: invocation.tool_name().as_str().to_owned(),
            facts: AuditFacts::from_invocation(invocation),
            policy: policy.to_owned(),
            rule: rule.to_owned(),
            decision: Some(decision),
            approval_id: None,
            status,
            error_class: (decision == AuditDecision::Deny).then(|| "policy_denied".to_owned()),
            exit_code: None,
        });
    }

    pub(crate) fn record_approval_request(
        &self,
        run_id: &str,
        invocation: &ResolvedToolInvocation,
        approval_id: &str,
    ) {
        self.insert(AuditEntry {
            sequence: 0,
            timestamp_ms: 0,
            run_id: run_id.to_owned(),
            call_id: invocation.call_id().to_string(),
            tool_name: invocation.tool_name().as_str().to_owned(),
            facts: AuditFacts::from_invocation(invocation),
            policy: "approval_fallback".to_owned(),
            rule: "unmatched build invocation".to_owned(),
            decision: None,
            approval_id: Some(approval_id.to_owned()),
            status: AuditExecutionStatus::WaitingApproval,
            error_class: None,
            exit_code: None,
        });
    }

    pub(crate) fn record_approval_decision(
        &self,
        run_id: &str,
        call_id: &ToolCallId,
        decision: AuditDecision,
    ) {
        self.update_entry(run_id, call_id, |entry| {
            entry.decision = Some(decision);
            entry.status = match decision {
                AuditDecision::Allow => AuditExecutionStatus::Authorized,
                AuditDecision::Deny => AuditExecutionStatus::Denied,
            };
            entry.error_class =
                (decision == AuditDecision::Deny).then(|| "approval_denied".to_owned());
        });
    }

    pub(crate) fn record_approval_cancelled(&self, run_id: &str, call_id: &ToolCallId) {
        self.update_entry(run_id, call_id, |entry| {
            entry.status = AuditExecutionStatus::Cancelled;
            entry.error_class = Some("cancelled".to_owned());
        });
    }

    pub(crate) fn record_started(&self, run_id: &str, call_id: &ToolCallId) {
        self.update_entry(run_id, call_id, |entry| {
            entry.status = AuditExecutionStatus::Started;
        });
    }

    pub(crate) fn record_result(&self, run_id: &str, result: &ToolResult) {
        self.update_entry(run_id, &result.call_id, |entry| match result.status {
            ToolResultStatus::Success => {
                entry.status = AuditExecutionStatus::Completed;
                entry.exit_code = shell_exit_code(result);
            }
            ToolResultStatus::Error => {
                let error_class = classify_error(result);
                if error_class == "cancelled" {
                    entry.status = AuditExecutionStatus::Cancelled;
                } else if !matches!(
                    entry.status,
                    AuditExecutionStatus::Denied | AuditExecutionStatus::Cancelled
                ) {
                    entry.status = AuditExecutionStatus::Failed;
                }
                if entry.error_class.is_none() {
                    entry.error_class = Some(error_class);
                }
            }
        });
    }

    fn insert(&self, mut entry: AuditEntry) {
        let mut state = self.lock();
        state.next_sequence = state.next_sequence.saturating_add(1);
        entry.sequence = state.next_sequence;
        entry.timestamp_ms = unix_timestamp_ms();
        state.entries.push(entry);
    }

    fn update_entry(
        &self,
        run_id: &str,
        call_id: &ToolCallId,
        update: impl FnOnce(&mut AuditEntry),
    ) {
        let mut state = self.lock();
        if let Some(entry) = state
            .entries
            .iter_mut()
            .rev()
            .find(|entry| entry.run_id == run_id && entry.call_id == call_id.to_string())
        {
            update(entry);
        }
    }

    fn lock(&self) -> MutexGuard<'_, AuditState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn shell_exit_code(result: &ToolResult) -> Option<i32> {
    result
        .content
        .as_single_json()?
        .get("exit_code")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

fn classify_error(result: &ToolResult) -> String {
    if result
        .content
        .as_single_json()
        .and_then(|value| value.pointer("/error/details/type"))
        .and_then(serde_json::Value::as_str)
        == Some("timeout")
    {
        return "timeout".to_owned();
    }
    match result.content.as_single_text() {
        Some(text) if text.starts_with("invalid tool input:") => "invalid_input".to_owned(),
        Some(text) if text.contains("interrupted") || text.contains("operation cancelled") => {
            "cancelled".to_owned()
        }
        Some(text) if text.contains("path not found:") => "file_not_found".to_owned(),
        Some(text) if text.contains("unsupported text encoding:") => {
            "unsupported_encoding".to_owned()
        }
        Some(text) if text.contains("unsupported file type:") => "unsupported_file_type".to_owned(),
        Some(text) if text.contains("file is too large:") => "file_too_large".to_owned(),
        Some(text) if text.contains("file changed while editing:") => {
            "concurrent_modification".to_owned()
        }
        Some(text) if text.contains("search backend unavailable:") => {
            "search_backend_unavailable".to_owned()
        }
        Some(text) if text.contains("io error:") => "io".to_owned(),
        _ => "tool_error".to_owned(),
    }
}

fn unix_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}
