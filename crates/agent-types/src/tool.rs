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

#[derive(Clone, Debug, Error, Eq, PartialEq)]
/// 工具结果内容不满足规范 Part 约束。
pub enum ToolResultContentError {
    #[error("tool result content must contain at least one part")]
    EmptyParts,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
/// 工具图片引用不满足 Session 内稳定路径约束。
pub enum ToolImageReferenceError {
    #[error("tool image media type is not supported")]
    UnsupportedMediaType,
    #[error(
        "tool image relative path must be a lowercase SHA-256 name with the canonical extension"
    )]
    InvalidRelativePath,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
/// Session 私有 `tool-images/` 中的一份稳定图片引用。
pub struct ToolImageReference {
    relative_path: String,
    media_type: String,
}

impl ToolImageReference {
    /// 创建并校验 Session 内图片引用。
    pub fn new(
        relative_path: impl Into<String>,
        media_type: impl Into<String>,
    ) -> Result<Self, ToolImageReferenceError> {
        let relative_path = relative_path.into();
        let media_type = media_type.into();
        let extension = canonical_image_extension(&media_type)
            .ok_or(ToolImageReferenceError::UnsupportedMediaType)?;
        let expected_length = 64 + 1 + extension.len();
        let valid_hash = relative_path
            .as_bytes()
            .get(..64)
            .is_some_and(|hash| hash.iter().all(u8::is_ascii_hexdigit))
            && relative_path
                .as_bytes()
                .get(..64)
                .is_some_and(|hash| hash.iter().all(|byte| !byte.is_ascii_uppercase()));
        if relative_path.len() != expected_length
            || !valid_hash
            || relative_path.as_bytes().get(64) != Some(&b'.')
            || relative_path.get(65..) != Some(extension)
        {
            return Err(ToolImageReferenceError::InvalidRelativePath);
        }
        Ok(Self {
            relative_path,
            media_type,
        })
    }

    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }
}

impl<'de> Deserialize<'de> for ToolImageReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireReference {
            relative_path: String,
            media_type: String,
        }

        let value = WireReference::deserialize(deserializer)?;
        Self::new(value.relative_path, value.media_type).map_err(de::Error::custom)
    }
}

fn canonical_image_extension(media_type: &str) -> Option<&'static str> {
    match media_type {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
/// 返回给模型的一个有序工具结果 Part。
pub enum ToolResultPart {
    Text { text: String },
    Json { value: Value },
    Image { image: ToolImageReference },
}

impl ToolResultPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn json(value: Value) -> Self {
        Self::Json { value }
    }

    pub fn image(image: ToolImageReference) -> Self {
        Self::Image { image }
    }
}

impl Serialize for ToolResultPart {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap;

        let mut map = match self {
            Self::Text { .. } | Self::Json { .. } => serializer.serialize_map(Some(2))?,
            Self::Image { .. } => serializer.serialize_map(Some(3))?,
        };
        match self {
            Self::Text { text } => {
                map.serialize_entry("type", "text")?;
                map.serialize_entry("text", text)?;
            }
            Self::Json { value } => {
                map.serialize_entry("type", "json")?;
                map.serialize_entry("value", value)?;
            }
            Self::Image { image } => {
                map.serialize_entry("type", "image")?;
                map.serialize_entry("relative_path", image.relative_path())?;
                map.serialize_entry("media_type", image.media_type())?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ToolResultPart {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum WirePart {
            Text {
                text: String,
            },
            Json {
                value: Value,
            },
            Image {
                relative_path: String,
                media_type: String,
            },
        }

        match WirePart::deserialize(deserializer)? {
            WirePart::Text { text } => Ok(Self::text(text)),
            WirePart::Json { value } => Ok(Self::json(value)),
            WirePart::Image {
                relative_path,
                media_type,
            } => ToolImageReference::new(relative_path, media_type)
                .map(Self::image)
                .map_err(de::Error::custom),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
/// 非空、有序的工具结果内容。
pub struct ToolResultContent {
    parts: Vec<ToolResultPart>,
}

impl ToolResultContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            parts: vec![ToolResultPart::text(text)],
        }
    }

    pub fn json(value: Value) -> Self {
        Self {
            parts: vec![ToolResultPart::json(value)],
        }
    }

    pub fn parts(parts: Vec<ToolResultPart>) -> Result<Self, ToolResultContentError> {
        if parts.is_empty() {
            return Err(ToolResultContentError::EmptyParts);
        }
        Ok(Self { parts })
    }

    pub fn as_parts(&self) -> &[ToolResultPart] {
        &self.parts
    }

    pub fn into_parts(self) -> Vec<ToolResultPart> {
        self.parts
    }

    pub fn as_single_text(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [ToolResultPart::Text { text }] => Some(text),
            _ => None,
        }
    }

    pub fn as_single_json(&self) -> Option<&Value> {
        match self.parts.as_slice() {
            [ToolResultPart::Json { value }] => Some(value),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for ToolResultContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum WireContent {
            Parts { parts: Vec<ToolResultPart> },
            Legacy(LegacyContent),
        }

        #[derive(Deserialize)]
        #[serde(tag = "type", content = "value", rename_all = "snake_case")]
        enum LegacyContent {
            Text(String),
            Json(Value),
        }

        match WireContent::deserialize(deserializer)? {
            WireContent::Parts { parts } => Self::parts(parts).map_err(de::Error::custom),
            WireContent::Legacy(LegacyContent::Text(text)) => Ok(Self::text(text)),
            WireContent::Legacy(LegacyContent::Json(value)) => Ok(Self::json(value)),
        }
    }
}

/// 不发送给模型、但随可靠 Tool Result 保存的执行观测信息。
///
/// 这里只承载跨工具通用且已经脱敏的事实；工具业务输出仍由
/// [`ToolResultContent`] 表达，Provider Adapter 必须忽略本字段。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolExecutionMetadata {
    /// 执行该工具内部模型调用的配置标识；普通工具保持为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_key: Option<String>,
    /// 工具执行内部调用的墙钟耗时。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// 工具内部模型调用由 Provider 确认的 token 用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::TokenUsage>,
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
    /// 不进入模型上下文的可选执行观测信息。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Box<ToolExecutionMetadata>>,
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
            content: ToolResultContent::json(serde_json::json!({"ok": true})),
            metadata: None,
        };
        let json = serde_json::to_string(&result).expect("serialize result");
        assert_eq!(
            serde_json::from_str::<ToolResult>(&json).expect("deserialize result"),
            result
        );
    }

    #[test]
    fn tool_result_content_reads_legacy_and_writes_parts() {
        let legacy = r#"{"type":"text","value":"done"}"#;
        let content: ToolResultContent = serde_json::from_str(legacy).expect("legacy content");
        assert_eq!(content.as_single_text(), Some("done"));
        assert_eq!(
            serde_json::to_value(content).expect("serialize content"),
            serde_json::json!({"parts": [{"type": "text", "text": "done"}]})
        );
    }

    #[test]
    fn tool_result_content_preserves_mixed_part_order() {
        let image = ToolImageReference::new(format!("{}.png", "a".repeat(64)), "image/png")
            .expect("valid image reference");
        let content = ToolResultContent::parts(vec![
            ToolResultPart::text("before"),
            ToolResultPart::image(image.clone()),
            ToolResultPart::json(serde_json::json!({"ok": true})),
        ])
        .expect("non-empty parts");
        let json = serde_json::to_value(&content).expect("serialize parts");
        assert_eq!(
            json,
            serde_json::json!({
                "parts": [
                    {"type": "text", "text": "before"},
                    {"type": "image", "relative_path": format!("{}.png", "a".repeat(64)), "media_type": "image/png"},
                    {"type": "json", "value": {"ok": true}}
                ]
            })
        );
        assert_eq!(
            serde_json::from_value::<ToolResultContent>(json).expect("deserialize parts"),
            content
        );
    }

    #[test]
    fn tool_image_reference_rejects_paths_and_mime_extension_mismatch() {
        let hash = "a".repeat(64);
        assert!(ToolImageReference::new(format!("{hash}.jpg"), "image/jpeg").is_ok());
        assert!(ToolImageReference::new(format!("{hash}.jpeg"), "image/jpeg").is_err());
        assert!(ToolImageReference::new(format!("../{hash}.png"), "image/png").is_err());
        assert!(ToolImageReference::new(format!("{}.png", "A".repeat(64)), "image/png").is_err());
        assert!(ToolImageReference::new(format!("{hash}.png"), "image/jpeg").is_err());
        assert!(ToolImageReference::new(format!("{hash}.bmp"), "image/bmp").is_err());
    }

    #[test]
    fn empty_tool_result_parts_are_rejected_during_construction_and_deserialization() {
        assert_eq!(
            ToolResultContent::parts(Vec::new()),
            Err(ToolResultContentError::EmptyParts)
        );
        assert!(serde_json::from_str::<ToolResultContent>(r#"{"parts":[]}"#).is_err());
    }
}
