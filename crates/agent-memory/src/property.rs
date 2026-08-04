use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// 可提供给模型理解的记忆属性值。
///
/// 使用 untagged JSON 表达，模型工具与持久化层只会看到字符串或数字；布尔、数组、对象
/// 和 null 在反序列化阶段直接失败。
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MemoryPropertyValue {
    /// 业务语义字符串。
    String(String),
    /// JSON 可表示的有限数字。
    Number(serde_json::Number),
}

impl MemoryPropertyValue {
    /// 属性值在 XML 与校验阶段使用的稳定类型名称。
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::String(_) => "string",
            Self::Number(_) => "number",
        }
    }

    /// 将属性值转换为模型可见文本。
    pub(crate) fn text(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Number(value) => value.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_property_accepts_only_strings_and_numbers() {
        assert_eq!(
            serde_json::from_value::<MemoryPropertyValue>(serde_json::json!("desktop"))
                .expect("string property"),
            MemoryPropertyValue::String("desktop".to_owned())
        );
        assert_eq!(
            serde_json::from_value::<MemoryPropertyValue>(serde_json::json!(42))
                .expect("number property"),
            MemoryPropertyValue::Number(serde_json::Number::from(42))
        );

        for invalid in [
            serde_json::json!(true),
            serde_json::json!(["desktop"]),
            serde_json::json!({"kind": "desktop"}),
            serde_json::Value::Null,
        ] {
            assert!(
                serde_json::from_value::<MemoryPropertyValue>(invalid).is_err(),
                "non string/number property must fail"
            );
        }
    }
}
