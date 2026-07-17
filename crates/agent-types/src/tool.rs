use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use thiserror::Error;

use crate::ToolCallId;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
/// ToolName 不满足模型工具命名规则时返回的错误。
pub enum ToolNameError {
    #[error("tool name must contain between 1 and 64 ASCII characters")]
    InvalidLength,
    #[error("tool name must start with an ASCII letter")]
    InvalidStart,
    #[error("tool name may only contain ASCII letters, digits, underscores, and hyphens")]
    InvalidCharacter,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
/// 模型可见的工具名称。
///
/// 使用 newtype 而不是裸 `String`，确保名称在进入 Provider Adapter 前已经完成校验。
pub struct ToolName(String);

impl ToolName {
    /// 创建工具名称。
    ///
    /// 名称长度为 1—64，只能以 ASCII 字母开头，并包含字母、数字、`_` 或 `-`。
    pub fn new(value: impl Into<String>) -> Result<Self, ToolNameError> {
        let value = value.into();
        if value.is_empty() || value.len() > 64 {
            return Err(ToolNameError::InvalidLength);
        }
        let mut bytes = value.bytes();
        let Some(first) = bytes.next() else {
            return Err(ToolNameError::InvalidLength);
        };
        if !first.is_ascii_alphabetic() {
            return Err(ToolNameError::InvalidStart);
        }
        if !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')) {
            return Err(ToolNameError::InvalidCharacter);
        }
        Ok(Self(value))
    }

    /// 读取工具名称字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for ToolName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ToolName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // 与 ID 相同，外部 JSON 也不能绕过 ToolName 的格式校验。
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 暴露给模型的工具定义。
pub struct ToolDefinition {
    /// Provider 请求中使用的工具名称。
    pub name: ToolName,
    /// 面向模型的工具用途说明。
    pub description: String,
    /// 工具输入参数的 JSON Schema。
    pub input_schema: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "name", rename_all = "snake_case")]
/// 本次模型调用应如何选择工具。
pub enum ToolChoice {
    /// 由模型决定是否调用以及调用哪个工具。
    Auto,
    /// 禁止本次调用使用工具。
    None,
    /// 要求模型至少调用一个工具，但不限定名称。
    Required,
    /// 强制模型调用指定工具。
    Named(ToolName),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// 工具执行结果是成功还是失败。
pub enum ToolResultStatus {
    Success,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
/// 返回给模型的工具结果内容。
pub enum ToolResultContent {
    /// 普通文本结果。
    Text(String),
    /// 需要保持结构的 JSON 结果。
    Json(Value),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 一次工具调用的规范结果。
pub struct ToolResult {
    /// 对应 Tool Call 的 ID，Provider 用它恢复调用链。
    pub call_id: ToolCallId,
    /// 工具是否执行成功。
    pub status: ToolResultStatus,
    /// 返回给模型的内容。
    pub content: ToolResultContent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_enforce_provider_safe_shape() {
        assert!(ToolName::new("read_file").is_ok());
        assert!(ToolName::new("1read").is_err());
        assert!(ToolName::new("read.file").is_err());
        assert!(ToolName::new("a".repeat(65)).is_err());
    }

    #[test]
    fn tool_result_round_trips_with_call_id() {
        let result = ToolResult {
            call_id: ToolCallId::new("call_1").expect("valid call id"),
            status: ToolResultStatus::Success,
            content: ToolResultContent::Json(serde_json::json!({"ok": true})),
        };
        let json = serde_json::to_string(&result).expect("serialize result");
        assert_eq!(
            serde_json::from_str::<ToolResult>(&json).expect("deserialize result"),
            result
        );
    }
}
