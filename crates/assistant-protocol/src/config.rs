//! 可安全跨进程展示的 Runtime 配置状态与模型投影。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 当前配置源可供 Runtime 使用的程度。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationState {
    /// 配置文件不存在。
    Missing,
    /// 配置无法形成有效快照。
    Invalid,
    /// 已形成快照，但部分模型或默认模型不可用。
    Degraded,
    /// 默认模型与全部模型均有效。
    Ready,
}

/// 配置诊断的稳定、脱敏分类。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationIssueCode {
    /// TOML 语法或重复字段错误。
    InvalidSyntax,
    /// schema version 不受当前 Runtime 支持。
    UnsupportedSchemaVersion,
    /// 顶层或全局配置无法解释。
    InvalidTopLevel,
    /// 配置文件类型、权限或大小无法安全处理。
    UnsafeConfigSource,
    /// 配置文件无法读取或解码。
    ConfigReadFailed,
    /// 存在当前 schema 未定义的字段。
    UnknownField,
    /// 必填字段缺失。
    MissingField,
    /// token、超时或执行上限无效。
    InvalidLimit,
    /// Runtime 或 Agent 全局策略无效。
    InvalidPolicy,
}

/// 一条不包含原始 TOML、credential 或底层错误正文的诊断。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ConfigurationIssue {
    /// 稳定诊断分类。
    pub code: ConfigurationIssueCode,
    /// 已脱敏、可直接展示的诊断文本。
    pub message: String,
}

/// 配置总体状态；模型明细通过独立命令查询。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ConfigurationStatus {
    /// Host 允许展示的配置文件路径；抽象测试源可以没有路径。
    pub config_path: Option<String>,
    /// 当前原始配置文档的内容修订；文件缺失时为空。
    pub revision: Option<String>,
    /// 当前配置可用程度。
    pub state: ConfigurationState,
    /// 成功读取到的 schema version。
    pub schema_version: Option<u32>,
    /// 不归属于单个合法 model key 的全局诊断。
    pub issues: Vec<ConfigurationIssue>,
}
