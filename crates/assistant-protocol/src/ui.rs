//! 插件 UI 声明与跨层入口身份；声明不授予页面能力。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::McpUiCsp;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum PluginUiEntryKind {
    Card,
    App,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginUiResource {
    Package { path: String },
    Mcp { server: String, uri: String },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "kebab-case")]
pub enum PluginUiCapability {
    SaveSnapshot,
    OpenApp,
    PostMessage,
    SetComposerDraft,
    CallTool,
}

/// format_version=2 的入口声明；只有按声明用途挂载的实例才能协商已实现能力。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(deny_unknown_fields)]
pub struct PluginUiEntry {
    pub id: String,
    pub title: String,
    pub kind: PluginUiEntryKind,
    pub resource: PluginUiResource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon: Option<String>,
    #[ts(type = "unknown")]
    pub input_schema: Value,
    #[serde(default)]
    pub capabilities: Vec<PluginUiCapability>,
    #[serde(default)]
    pub csp: McpUiCsp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub snapshot_schema: Option<String>,
    #[serde(default)]
    pub readable_snapshot_schemas: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(deny_unknown_fields)]
pub struct PluginUi {
    pub entries: Vec<PluginUiEntry>,
}

/// 应用入口身份，不含 HTML、凭据或活动页面权限。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiAppTarget {
    Plugin {
        name: String,
        entry_id: String,
    },
    Mcp {
        target: crate::McpServerTarget,
        resource_uri: String,
        entry_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UiAppSummary {
    pub target: UiAppTarget,
    pub entry: PluginUiEntry,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListUiAppsResult {
    pub apps: Vec<UiAppSummary>,
    /// 部分来源读取失败时仍保留成功项；不把失败伪装为空目录。
    pub unavailable_sources: Vec<String>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReadUiAppRequest {
    pub target: UiAppTarget,
    #[ts(type = "Record<string, unknown>")]
    pub arguments: std::collections::BTreeMap<String, Value>,
    /// 受信客户端只在挂载应用时提供作用域。定位检查可以省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mount: Option<UiAppScope>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReadUiAppResult {
    /// 当前连接／包与设置身份，用于复用页面前核对来源。
    pub source_identity: String,
    pub resource: crate::McpUiResource,
    pub entry: PluginUiEntry,
    /// 已挂载插件入口的包内图标，仅供客户端作为图片显示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon_data_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool_instance_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiAppScope {
    Draft,
    Session { session_id: crate::SessionId },
}

/// 受信客户端绑定的页面句柄和请求生命周期。内嵌页面只提供工具名和参数。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UiToolRequest {
    pub instance_id: String,
    pub action: UiToolAction,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiToolAction {
    Start {
        request_id: String,
        name: String,
        #[ts(type = "Record<string, unknown>")]
        arguments: std::collections::BTreeMap<String, Value>,
        /// 受信客户端为当前沙箱的单次真实点击提供的事实。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        user_gesture: Option<bool>,
    },
    Poll {
        request_id: String,
    },
    Decide {
        request_id: String,
        approval_id: crate::ApprovalId,
        decision: crate::ApprovalDecision,
    },
    Cancel {
        request_id: String,
    },
    Release,
}

/// 页面审批没有 Run，来源和会话作用域由实际挂载句柄决定。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UiToolApproval {
    pub approval_id: crate::ApprovalId,
    pub app_title: String,
    pub scope: UiAppScope,
    pub input: crate::ToolInputProjection,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiToolState {
    Pending,
    Approval {
        approval: UiToolApproval,
    },
    Complete {
        #[ts(type = "unknown")]
        result: Value,
    },
    Failed {
        reason: String,
        remote_may_have_executed: bool,
    },
    Acknowledged,
}

macro_rules! private_payload_debug {
    ($($ty:ty),+ $(,)?) => { $(
        impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($ty)).finish_non_exhaustive()
            }
        }
    )+ };
}
private_payload_debug!(UiToolRequest, UiToolState);

impl std::fmt::Debug for ReadUiAppRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadUiAppRequest")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for ReadUiAppResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadUiAppResult").finish_non_exhaustive()
    }
}
