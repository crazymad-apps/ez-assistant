use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{kind} must not be empty")]
/// 创建或反序列化领域 ID 时返回的校验错误。
pub struct IdentifierError {
    kind: &'static str,
}

// Rust 的 newtype 是“只包一层的专用类型”。例如 ToolCallId 和 MessageId 底层都是 String，
// 但编译器不会允许把它们互相误传。这个宏用来生成多种行为一致的 ID newtype，
// 避免为每个 ID 重复编写构造、显示和 Serde 序列化代码。
macro_rules! identifier {
    ($doc:literal, $name:ident, $kind:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// 从字符串创建 ID。空字符串或纯空白字符串会被拒绝。
            pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(IdentifierError { kind: $kind });
                }
                Ok(Self(value))
            }

            /// 以借用形式读取内部字符串，不转移 ID 的所有权。
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// 取出内部字符串，同时消费当前 ID。
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
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

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                // 反序列化也必须经过 `new`，防止 JSON 绕过“ID 不得为空”的不变量。
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

identifier!("一条对话消息的标识。", MessageId, "message id");
identifier!("Assistant 响应中一个内容片段的标识。", PartId, "part id");
identifier!(
    "一次模型工具调用的标识，用来把 Tool Call 与 Tool Result 严格配对。",
    ToolCallId,
    "tool call id"
);
identifier!("模型服务提供方的标识。", ProviderId, "provider id");
identifier!(
    "Provider 使用的具体通信协议标识。",
    ProtocolId,
    "protocol id"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_reject_empty_values() {
        assert!(ToolCallId::new("").is_err());
        assert!(ProviderId::new("   ").is_err());
    }

    #[test]
    fn identifiers_round_trip_as_strings() {
        let id = ToolCallId::new("call_123").expect("valid id");
        let json = serde_json::to_string(&id).expect("serialize id");
        assert_eq!(json, r#""call_123""#);
        assert_eq!(
            serde_json::from_str::<ToolCallId>(&json).expect("deserialize id"),
            id
        );
    }

    #[test]
    fn deserialization_enforces_identifier_invariants() {
        assert!(serde_json::from_str::<MessageId>(r#"""#).is_err());
    }
}
