use std::{collections::BTreeMap, fmt, num::NonZeroUsize, str::FromStr};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use crate::MemoryPropertyValue;

/// 一个可检索记忆数据源的稳定标识。
#[derive(Clone, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd)]
pub struct RecallSourceId(String);

impl RecallSourceId {
    /// 创建 Source ID；空白和控制字符会被拒绝，容量由协调器显式配置。
    pub fn new(value: impl Into<String>) -> Result<Self, MemoryRecallError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(MemoryRecallError::invalid_input(
                "recall source id must not be blank",
            ));
        }
        if value.chars().any(char::is_control) {
            return Err(MemoryRecallError::invalid_input(
                "recall source id contains a disallowed control character",
            ));
        }
        Ok(Self(value))
    }

    /// 借用内部字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 消费当前值并取回内部字符串。
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for RecallSourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for RecallSourceId {
    type Err = MemoryRecallError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for RecallSourceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RecallSourceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// 统一召回能力接收的模型检索意图。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecallRequest {
    /// 非空检索文本。
    pub query: String,
    /// 模型明确要求返回的最大结果数。
    pub limit: NonZeroUsize,
    /// 指定 Source；`None` 表示使用协调器的显式默认集合。
    pub sources: Option<Vec<RecallSourceId>>,
}

/// 单个 RecallSource 实际接收的最小请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecallSourceRequest {
    /// 与统一请求相同的检索文本。
    pub query: String,
    /// 该 Source 最多应返回的候选数。
    pub limit: NonZeroUsize,
}

/// 单个 Source 返回、尚未附加可信来源的候选条目。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecallSourceItem {
    /// 模型可直接阅读的召回正文。
    pub content: String,
    /// 帮助模型理解结果业务含义的字符串或数字属性。
    pub attributes: BTreeMap<String, MemoryPropertyValue>,
    /// Source 内部可稳定定位该结果的可选引用，不包含实现路径或凭据。
    pub reference: Option<String>,
}

/// 单个 Source 已按自身相关性排序的结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecallSourceResponse {
    /// 按相关性从高到低排列的候选。
    pub items: Vec<RecallSourceItem>,
    /// Source 是否还有候选因请求上限而未返回。
    pub truncated: bool,
}

/// 协调器为一条结果附加的可信来源。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecallOrigin {
    /// 产生该结果的 Source ID。
    pub source_id: RecallSourceId,
    /// Source 提供的可选稳定引用。
    pub reference: Option<String>,
}

/// 返回给统一调用方的召回条目。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecallItem {
    /// 模型可直接阅读的召回正文。
    pub content: String,
    /// 协调器附加的一个或多个可信来源。
    pub origins: Vec<RecallOrigin>,
    /// 帮助模型理解结果业务含义的字符串或数字属性。
    pub attributes: BTreeMap<String, MemoryPropertyValue>,
}

/// 单个 Source 失败的稳定类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallFailureKind {
    /// Source 当前不可用。
    Unavailable,
    /// Source 超过允许时间。
    Timeout,
    /// Source I/O 失败。
    Io,
    /// Source 返回了违反契约的数据。
    InvalidData,
    /// Source 调用被取消。
    Cancelled,
    /// Source 内部失败，且没有更具体的稳定分类。
    Internal,
}

/// 一个 Source 的结构化失败信息。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecallFailure {
    /// 失败 Source 的稳定 ID。
    pub source_id: RecallSourceId,
    /// 稳定失败类别。
    pub kind: RecallFailureKind,
    /// 不包含正文、实现路径或凭据的诊断信息。
    pub message: String,
}

/// 多 Source 协调后的统一响应。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecallResponse {
    /// 已按确定规则合并、去重和截断的结果。
    pub items: Vec<RecallItem>,
    /// 未阻止其他有效结果返回的 Source 级失败。
    pub failures: Vec<MemoryRecallFailure>,
    /// 是否还有候选因 Source 或统一请求上限而未返回。
    pub truncated: bool,
}

/// 统一 Memory Recall 调用失败。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum MemoryRecallError {
    /// 请求不满足稳定输入约束。
    #[error("invalid memory recall input: {message}")]
    InvalidInput {
        /// 不包含完整 query 的受控诊断。
        message: String,
    },
    /// 所有选中的 Source 均失败。
    #[error("all selected recall sources failed")]
    AllSourcesFailed {
        /// 按 Source 构造顺序排列的失败明细。
        failures: Vec<MemoryRecallFailure>,
    },
    /// 整体调用已被取消。
    #[error("memory recall was cancelled")]
    Cancelled,
}

impl MemoryRecallError {
    pub(crate) fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_source_id_rejects_blank_control_and_serde_bypass() {
        assert!(RecallSourceId::new(" ").is_err());
        assert!(RecallSourceId::new("notes\0private").is_err());
        assert!(serde_json::from_str::<RecallSourceId>(r#"""#).is_err());

        let id = RecallSourceId::new("notes").expect("valid source id");
        assert_eq!(serde_json::to_string(&id).expect("serialize"), r#""notes""#);
        assert_eq!(
            serde_json::from_str::<RecallSourceId>(r#""notes""#).expect("deserialize"),
            id
        );
    }
}
