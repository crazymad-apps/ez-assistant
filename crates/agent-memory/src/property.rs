use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Pinned Memory 的 Runtime 业务属性值。
///
/// 使用 untagged JSON 表达，业务接口与持久化层只接受字符串或数字；该值不属于记忆正文，
/// 不进入模型工具投影或 System Context。布尔、数组、对象和 null 在反序列化阶段直接失败。
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MemoryPropertyValue {
    /// 业务语义字符串。
    String(String),
    /// JSON 可表示的有限数字。
    Number(serde_json::Number),
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
