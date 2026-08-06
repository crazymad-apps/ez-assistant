//! 应用层不透明标识。

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;

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
    /// Runtime 中一次业务 Run 的不透明标识。
    RunId,
    "run_id"
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
    fn every_identifier_rejects_empty_or_whitespace_only_values() {
        assert_eq!(
            SessionId::new(" ")
                .expect_err("whitespace must fail")
                .kind(),
            "session_id"
        );
        assert!(RunId::new("").is_err());
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
}
