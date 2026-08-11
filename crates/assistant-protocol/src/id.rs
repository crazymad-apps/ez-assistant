//! 应用层不透明标识。

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;

const MAX_MODEL_KEY_BYTES: usize = 64;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;

/// 应用层标识为空或只包含空白字符。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{kind} must not be empty")]
pub struct IdentifierError {
    kind: &'static str,
}

impl IdentifierError {
    /// 返回发生校验错误的标识类别。
    pub fn kind(&self) -> &'static str {
        self.kind
    }
}

fn validate(value: String, kind: &'static str) -> Result<String, IdentifierError> {
    if value.trim().is_empty() {
        Err(IdentifierError { kind })
    } else {
        Ok(value)
    }
}

macro_rules! define_identifier {
    ($(#[$meta:meta])* $name:ident, $kind:literal) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// 校验并构造标识；原始非空内容保持不变。
            pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
                validate(value.into(), $kind).map(Self)
            }

            /// 以字符串切片读取不透明标识。
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = IdentifierError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdentifierError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(D::Error::custom)
            }
        }
    };
}

define_identifier!(
    /// Runtime 中一个会话的不透明标识。
    SessionId,
    "session_id"
);
define_identifier!(
    /// Runtime 中一个 Workspace 的不透明标识。
    WorkspaceId,
    "workspace_id"
);
define_identifier!(
    /// Runtime 中一个 Session Attachment 的不透明标识。
    AttachmentId,
    "attachment_id"
);
define_identifier!(
    /// Runtime 中一次业务 Run 的不透明标识。
    RunId,
    "run_id"
);
define_identifier!(
    /// Runtime 中一次已接受用户输入的不透明标识。
    InputId,
    "input_id"
);
define_identifier!(
    /// 应用层规范消息的不透明标识。
    MessageId,
    "message_id"
);
define_identifier!(
    /// 应用层消息片段的不透明标识。
    PartId,
    "part_id"
);
define_identifier!(
    /// 应用层工具调用的不透明标识。
    ToolCallId,
    "tool_call_id"
);

/// 客户端提交输入时使用的不透明请求身份；只在同一 Session 内比较。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// 校验非空且不超过 128 字节；原始内容不做归一化。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = validate(value.into(), "idempotency_key")?;
        if value.len() > MAX_IDEMPOTENCY_KEY_BYTES {
            return Err(IdentifierError {
                kind: "idempotency_key",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for IdempotencyKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

/// 用户配置中模型条目的稳定 key 校验错误。
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ModelKeyError {
    /// key 不能为空。
    #[error("model_key must not be empty")]
    Empty,
    /// key 最多包含 64 个 ASCII 字节。
    #[error("model_key must not exceed 64 ASCII characters")]
    TooLong,
    /// 首字符必须是 ASCII 字母或数字。
    #[error("model_key must start with an ASCII letter or digit")]
    InvalidStart,
    /// 后续字符只能是 ASCII 字母、数字、连字符或下划线。
    #[error("model_key may contain only ASCII letters, digits, hyphens, and underscores")]
    InvalidCharacter,
}

/// 用户为模型配置指定的稳定业务 key；它不是 Runtime 生成的内部 ID。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ModelKey(String);

impl ModelKey {
    /// 按 `[A-Za-z0-9][A-Za-z0-9_-]{0,63}` 校验并构造 key。
    pub fn new(value: impl Into<String>) -> Result<Self, ModelKeyError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let Some(first) = bytes.first() else {
            return Err(ModelKeyError::Empty);
        };
        if bytes.len() > MAX_MODEL_KEY_BYTES {
            return Err(ModelKeyError::TooLong);
        }
        if !first.is_ascii_alphanumeric() {
            return Err(ModelKeyError::InvalidStart);
        }
        if !bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(ModelKeyError::InvalidCharacter);
        }
        Ok(Self(value))
    }

    /// 读取配置 key 的原始字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ModelKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ModelKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ModelKey {
    type Err = ModelKeyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ModelKey {
    type Error = ModelKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ModelKey> for String {
    fn from(value: ModelKey) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for ModelKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_keeps_opaque_content_and_uses_transparent_json() {
        let id = SessionId::new(" session-1 ").expect("non-empty identifier");

        assert_eq!(id.as_str(), " session-1 ");
        assert_eq!(
            serde_json::to_string(&id).expect("serialize"),
            "\" session-1 \""
        );
        assert_eq!(
            serde_json::from_str::<SessionId>("\" session-1 \"").expect("deserialize"),
            id
        );
    }

    #[test]
    fn idempotency_key_is_nonempty_bounded_and_opaque() {
        let key = IdempotencyKey::new(" request-1 ").expect("key");
        assert_eq!(key.as_str(), " request-1 ");
        assert!(IdempotencyKey::new(" ").is_err());
        assert!(IdempotencyKey::new("x".repeat(MAX_IDEMPOTENCY_KEY_BYTES + 1)).is_err());
        assert_eq!(
            serde_json::from_str::<IdempotencyKey>(
                &serde_json::to_string(&key).expect("serialize")
            )
            .expect("deserialize"),
            key
        );
    }

    #[test]
    fn every_identifier_rejects_empty_or_whitespace_only_values() {
        assert_eq!(
            SessionId::new(" ")
                .expect_err("whitespace must fail")
                .kind(),
            "session_id"
        );
        assert!(RunId::new("").is_err());
        assert!(WorkspaceId::new(" ").is_err());
        assert!(InputId::new(" ").is_err());
        assert!(MessageId::new("\n").is_err());
        assert!(PartId::new("\t").is_err());
        assert!(ToolCallId::new("  ").is_err());
        assert!(serde_json::from_str::<RunId>("\"\"").is_err());
    }

    #[test]
    fn every_identifier_has_the_same_transparent_wire_shape() {
        assert_eq!(
            serde_json::to_value(SessionId::new("session-1").expect("session id"))
                .expect("serialize session id"),
            "session-1"
        );
        assert_eq!(
            serde_json::to_value(RunId::new("run-1").expect("run id")).expect("serialize run id"),
            "run-1"
        );
        assert_eq!(
            serde_json::to_value(WorkspaceId::new("workspace-1").expect("workspace id"))
                .expect("serialize workspace id"),
            "workspace-1"
        );
        assert_eq!(
            serde_json::to_value(InputId::new("input-1").expect("input id"))
                .expect("serialize input id"),
            "input-1"
        );
        assert_eq!(
            serde_json::to_value(MessageId::new("message-1").expect("message id"))
                .expect("serialize message id"),
            "message-1"
        );
        assert_eq!(
            serde_json::to_value(PartId::new("part-1").expect("part id"))
                .expect("serialize part id"),
            "part-1"
        );
        assert_eq!(
            serde_json::to_value(ToolCallId::new("call-1").expect("tool call id"))
                .expect("serialize tool call id"),
            "call-1"
        );
    }

    #[test]
    fn model_key_enforces_the_configuration_grammar() {
        for valid in ["a", "A1", "deepseek-chat", "local_model"] {
            let key = ModelKey::new(valid).expect("valid model key");
            assert_eq!(key.as_str(), valid);
        }

        assert_eq!(ModelKey::new("").unwrap_err(), ModelKeyError::Empty);
        assert_eq!(
            ModelKey::new("-model").unwrap_err(),
            ModelKeyError::InvalidStart
        );
        assert_eq!(
            ModelKey::new("model.key").unwrap_err(),
            ModelKeyError::InvalidCharacter
        );
        assert_eq!(
            ModelKey::new("模型").unwrap_err(),
            ModelKeyError::InvalidStart
        );
        assert_eq!(
            ModelKey::new("a".repeat(65)).unwrap_err(),
            ModelKeyError::TooLong
        );
    }

    #[test]
    fn model_key_uses_a_validated_transparent_wire_shape() {
        let key = ModelKey::new("deepseek-chat").expect("valid model key");
        assert_eq!(
            serde_json::to_string(&key).expect("serialize"),
            "\"deepseek-chat\""
        );
        assert_eq!(
            serde_json::from_str::<ModelKey>("\"deepseek-chat\"").expect("deserialize"),
            key
        );
        assert!(serde_json::from_str::<ModelKey>("\"invalid key\"").is_err());
    }
}
