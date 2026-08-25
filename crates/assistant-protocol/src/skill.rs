//! 本地 Skill 的脱敏产品投影与激活标签。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::MessageId;

/// Skill 候选所属的固定扫描来源。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SkillSourceSnapshot {
    WorkspaceEzAssistant,
    WorkspaceAgents,
    UserEzAssistant,
    UserAgents,
}

/// 当前管理投影中一个名称的可用状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SkillHealthSnapshot {
    Ready,
    Disabled,
    Conflict,
    Unavailable,
}

/// 设置页或 Session Catalog 使用的一项脱敏 Skill 摘要。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SkillSummarySnapshot {
    pub name: String,
    pub description: String,
    pub source: SkillSourceSnapshot,
    pub model_invocable: bool,
    pub user_invocable: bool,
    pub enabled: bool,
    pub health: SkillHealthSnapshot,
}

/// Skill 诊断严重级别。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SkillDiagnosticSeveritySnapshot {
    Warning,
    Error,
}

/// 不携带正文或绝对路径的 Skill 诊断。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SkillDiagnosticSnapshot {
    pub severity: SkillDiagnosticSeveritySnapshot,
    pub code: String,
    pub skill_name: Option<String>,
    pub source: Option<SkillSourceSnapshot>,
    pub detail: String,
}

/// 设置页每次显式读取到的当前管理投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SkillManagementSnapshot {
    pub available: bool,
    pub skills: Vec<SkillSummarySnapshot>,
    pub diagnostics: Vec<SkillDiagnosticSnapshot>,
}

/// 设置页按需读取的一项当前生效 Skill 详情。
///
/// 正文只属于管理详情，不进入 Session、Composer 或列表摘要投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SkillDetailSnapshot {
    pub skill: SkillSummarySnapshot,
    /// 同名同层冲突或扫描不完整时没有可确定展示的当前正文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub body: Option<String>,
    pub diagnostics: Vec<SkillDiagnosticSnapshot>,
}

/// Session Catalog 的冻结状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SessionSkillCatalogStatusSnapshot {
    Ready,
    Empty,
    Unavailable,
    LegacyUnavailable,
}

/// Session 页面和 Composer 使用的冻结 Catalog 投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionSkillCatalogSnapshot {
    pub status: SessionSkillCatalogStatusSnapshot,
    pub skills: Vec<SkillSummarySnapshot>,
    pub diagnostics: Vec<SkillDiagnosticSnapshot>,
}

impl Default for SessionSkillCatalogSnapshot {
    fn default() -> Self {
        Self {
            status: SessionSkillCatalogStatusSnapshot::LegacyUnavailable,
            skills: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}

/// 一次冻结 Activation 的最小审计标签。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SkillActivationTagSnapshot {
    pub name: String,
}

/// Activation 的触发来源。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SkillActivationTriggerSnapshot {
    User,
    Model,
}

/// 当前 Conversation 中每个名称最新生效的 Activation。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ActiveSkillSnapshot {
    pub tag: SkillActivationTagSnapshot,
    pub trigger: SkillActivationTriggerSnapshot,
    pub message_id: MessageId,
    pub created_at_ms: i64,
}
