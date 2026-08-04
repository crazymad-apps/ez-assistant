use std::{collections::BTreeMap, fmt, num::NonZeroUsize, str::FromStr};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use crate::MemoryPropertyValue;

/// Pinned Memory 领域输入不满足稳定不变量。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PinnedMemoryValidationError {
    /// 必填文本为空或只包含空白。
    #[error("{field} must not be blank")]
    Blank {
        /// 出错字段的稳定名称。
        field: &'static str,
    },
    /// 文本包含该字段不允许的控制字符。
    #[error("{field} contains a disallowed control character")]
    ControlCharacter {
        /// 出错字段的稳定名称。
        field: &'static str,
    },
    /// UTF-8 字节数超过显式上限。
    #[error("{field} is {actual_bytes} bytes, exceeding the {max_bytes} byte limit")]
    TooLong {
        /// 出错字段的稳定名称。
        field: &'static str,
        /// 实际 UTF-8 字节数。
        actual_bytes: usize,
        /// 允许的最大 UTF-8 字节数。
        max_bytes: usize,
    },
    /// 条目数量超过显式上限。
    #[error("pinned memory has {actual} entries, exceeding the {max} entry limit")]
    TooManyEntries {
        /// 实际条目数。
        actual: usize,
        /// 允许的最大条目数。
        max: usize,
    },
    /// 单条记忆的属性数量超过显式上限。
    #[error("pinned memory has {actual} attributes, exceeding the {max} attribute limit")]
    TooManyAttributes {
        /// 实际属性数。
        actual: usize,
        /// 允许的最大属性数。
        max: usize,
    },
    /// Patch 没有表达任何修改。
    #[error("pinned memory patch must modify at least one field")]
    EmptyPatch,
}

macro_rules! validated_string_newtype {
    ($name:ident, $field:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// 创建值；空白或控制字符会在这里被拒绝，容量由 Limits 统一校验。
            pub fn new(value: impl Into<String>) -> Result<Self, PinnedMemoryValidationError> {
                let value = value.into();
                validate_required_text($field, &value, false)?;
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

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = PinnedMemoryValidationError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
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
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

validated_string_newtype!(
    PinnedMemoryId,
    "pinned memory id",
    "一条 Pinned Memory 的稳定标识。"
);
validated_string_newtype!(
    PinnedMemoryCategory,
    "pinned memory category",
    "Pinned Memory 的开放归类。"
);

/// Pinned Memory 的全部显式容量限制。
///
/// 所有字段使用 `NonZeroUsize`，因此不存在零值或隐藏默认；具体数值由宿主装配。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedMemoryLimits {
    /// 一个快照或 Store 最多包含的条目数。
    pub max_entries: NonZeroUsize,
    /// 单个稳定 ID 的最大 UTF-8 字节数。
    pub max_id_bytes: NonZeroUsize,
    /// 单个归类名称的最大 UTF-8 字节数。
    pub max_category_bytes: NonZeroUsize,
    /// 单条记忆正文的最大 UTF-8 字节数。
    pub max_content_bytes: NonZeroUsize,
    /// 单条记忆最多包含的属性数。
    pub max_attributes_per_entry: NonZeroUsize,
    /// 单个属性键的最大 UTF-8 字节数。
    pub max_attribute_key_bytes: NonZeroUsize,
    /// 单个字符串属性值的最大 UTF-8 字节数；数字不使用该限制。
    pub max_attribute_string_bytes: NonZeroUsize,
    /// Pinned Memory 整体说明的最大 UTF-8 字节数。
    pub max_description_bytes: NonZeroUsize,
    /// 最终 XML System Prompt Part 的最大 UTF-8 字节数。
    pub max_snapshot_bytes: NonZeroUsize,
}

/// 已保存的完整 Pinned Memory 条目。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PinnedMemoryEntry {
    /// Store 分配并长期保持稳定的条目标识。
    pub id: PinnedMemoryId,
    /// 用于归类和稳定排序的开放字符串。
    pub category: PinnedMemoryCategory,
    /// 模型需要长期看到的记忆正文。
    pub content: String,
    /// 帮助模型理解该条记忆业务含义的扩展属性。
    pub attributes: BTreeMap<String, MemoryPropertyValue>,
}

impl PinnedMemoryEntry {
    /// 使用宿主提供的完整 Limits 校验条目。
    pub fn validate(&self, limits: &PinnedMemoryLimits) -> Result<(), PinnedMemoryValidationError> {
        self.id.validate(limits)?;
        validate_category(&self.category, limits)?;
        validate_content(&self.content, limits)?;
        validate_attributes(&self.attributes, limits)
    }
}

impl PinnedMemoryId {
    /// 使用宿主显式 Limits 校验稳定 ID 的容量。
    pub fn validate(&self, limits: &PinnedMemoryLimits) -> Result<(), PinnedMemoryValidationError> {
        validate_length("pinned memory id", self.as_str(), limits.max_id_bytes)
    }
}

impl PinnedMemoryCategory {
    /// 使用宿主显式 Limits 校验归类名称的容量。
    pub fn validate(&self, limits: &PinnedMemoryLimits) -> Result<(), PinnedMemoryValidationError> {
        validate_category(self, limits)
    }
}

/// 添加 Pinned Memory 时使用的不带 ID 输入。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PinnedMemoryDraft {
    /// 新条目的开放归类；稳定 ID 由 Store 分配。
    pub category: PinnedMemoryCategory,
    /// 新条目的记忆正文。
    pub content: String,
    /// 新条目的模型可见扩展属性。
    pub attributes: BTreeMap<String, MemoryPropertyValue>,
}

impl PinnedMemoryDraft {
    /// 使用宿主提供的完整 Limits 校验新增输入。
    pub fn validate(&self, limits: &PinnedMemoryLimits) -> Result<(), PinnedMemoryValidationError> {
        validate_category(&self.category, limits)?;
        validate_content(&self.content, limits)?;
        validate_attributes(&self.attributes, limits)
    }
}

/// 修改 Pinned Memory 的部分字段。
///
/// `attributes: None` 表示保留；`attributes: Some(empty)` 表示显式清空。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PinnedMemoryPatch {
    /// 新归类；`None` 表示保持原值。
    pub category: Option<PinnedMemoryCategory>,
    /// 新正文；`None` 表示保持原值。
    pub content: Option<String>,
    /// 新属性全集；`None` 表示保持原值，空 Map 表示清空。
    pub attributes: Option<BTreeMap<String, MemoryPropertyValue>>,
}

impl PinnedMemoryPatch {
    /// Patch 是否没有表达任何修改。
    pub fn is_empty(&self) -> bool {
        self.category.is_none() && self.content.is_none() && self.attributes.is_none()
    }

    /// 校验 Patch 非空，并校验所有明确提供的字段。
    pub fn validate(&self, limits: &PinnedMemoryLimits) -> Result<(), PinnedMemoryValidationError> {
        if self.is_empty() {
            return Err(PinnedMemoryValidationError::EmptyPatch);
        }
        if let Some(category) = &self.category {
            validate_category(category, limits)?;
        }
        if let Some(content) = &self.content {
            validate_content(content, limits)?;
        }
        if let Some(attributes) = &self.attributes {
            validate_attributes(attributes, limits)?;
        }
        Ok(())
    }
}

pub(crate) fn validate_snapshot_input(
    description: &str,
    entries: &[PinnedMemoryEntry],
    limits: &PinnedMemoryLimits,
) -> Result<(), PinnedMemoryValidationError> {
    validate_required_text("pinned memory description", description, true)?;
    validate_length(
        "pinned memory description",
        description,
        limits.max_description_bytes,
    )?;
    if entries.len() > limits.max_entries.get() {
        return Err(PinnedMemoryValidationError::TooManyEntries {
            actual: entries.len(),
            max: limits.max_entries.get(),
        });
    }
    for entry in entries {
        entry.validate(limits)?;
    }
    Ok(())
}

fn validate_category(
    category: &PinnedMemoryCategory,
    limits: &PinnedMemoryLimits,
) -> Result<(), PinnedMemoryValidationError> {
    validate_length(
        "pinned memory category",
        category.as_str(),
        limits.max_category_bytes,
    )
}

fn validate_content(
    content: &str,
    limits: &PinnedMemoryLimits,
) -> Result<(), PinnedMemoryValidationError> {
    validate_required_text("pinned memory content", content, true)?;
    validate_length("pinned memory content", content, limits.max_content_bytes)
}

fn validate_attributes(
    attributes: &BTreeMap<String, MemoryPropertyValue>,
    limits: &PinnedMemoryLimits,
) -> Result<(), PinnedMemoryValidationError> {
    if attributes.len() > limits.max_attributes_per_entry.get() {
        return Err(PinnedMemoryValidationError::TooManyAttributes {
            actual: attributes.len(),
            max: limits.max_attributes_per_entry.get(),
        });
    }
    for (key, value) in attributes {
        validate_required_text("pinned memory attribute key", key, false)?;
        validate_length(
            "pinned memory attribute key",
            key,
            limits.max_attribute_key_bytes,
        )?;
        if let MemoryPropertyValue::String(value) = value {
            validate_required_text("pinned memory attribute string", value, true)?;
            validate_length(
                "pinned memory attribute string",
                value,
                limits.max_attribute_string_bytes,
            )?;
        }
    }
    Ok(())
}

fn validate_required_text(
    field: &'static str,
    value: &str,
    allow_xml_whitespace: bool,
) -> Result<(), PinnedMemoryValidationError> {
    if value.trim().is_empty() {
        return Err(PinnedMemoryValidationError::Blank { field });
    }
    if value.chars().any(|character| {
        character.is_control() && !(allow_xml_whitespace && matches!(character, '\n' | '\r' | '\t'))
    }) {
        return Err(PinnedMemoryValidationError::ControlCharacter { field });
    }
    Ok(())
}

fn validate_length(
    field: &'static str,
    value: &str,
    max: NonZeroUsize,
) -> Result<(), PinnedMemoryValidationError> {
    let actual_bytes = value.len();
    if actual_bytes > max.get() {
        return Err(PinnedMemoryValidationError::TooLong {
            field,
            actual_bytes,
            max_bytes: max.get(),
        });
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn limits() -> PinnedMemoryLimits {
        PinnedMemoryLimits {
            max_entries: NonZeroUsize::new(3).expect("non-zero"),
            max_id_bytes: NonZeroUsize::new(16).expect("non-zero"),
            max_category_bytes: NonZeroUsize::new(16).expect("non-zero"),
            max_content_bytes: NonZeroUsize::new(64).expect("non-zero"),
            max_attributes_per_entry: NonZeroUsize::new(2).expect("non-zero"),
            max_attribute_key_bytes: NonZeroUsize::new(16).expect("non-zero"),
            max_attribute_string_bytes: NonZeroUsize::new(32).expect("non-zero"),
            max_description_bytes: NonZeroUsize::new(64).expect("non-zero"),
            max_snapshot_bytes: NonZeroUsize::new(4096).expect("non-zero"),
        }
    }

    pub(crate) fn entry(id: &str, category: &str, content: &str) -> PinnedMemoryEntry {
        PinnedMemoryEntry {
            id: PinnedMemoryId::new(id).expect("valid id"),
            category: PinnedMemoryCategory::new(category).expect("valid category"),
            content: content.to_owned(),
            attributes: BTreeMap::new(),
        }
    }

    #[test]
    fn pinned_ids_and_categories_reject_blank_control_and_serde_bypass() {
        assert!(PinnedMemoryId::new(" ").is_err());
        assert!(PinnedMemoryId::new("id\0bad").is_err());
        assert!(PinnedMemoryCategory::new("\n").is_err());
        assert!(serde_json::from_str::<PinnedMemoryId>(r#"""#).is_err());

        let id = PinnedMemoryId::new("memory_1").expect("valid id");
        assert_eq!(
            serde_json::to_string(&id).expect("serialize"),
            r#""memory_1""#
        );
        assert_eq!(
            serde_json::from_str::<PinnedMemoryId>(r#""memory_1""#).expect("deserialize"),
            id
        );
    }

    #[test]
    fn pinned_entry_validates_every_explicit_limit() {
        let limits = limits();
        let mut value = entry("memory_1", "preference", "Use dark mode");
        value.attributes.insert(
            "scope".to_owned(),
            MemoryPropertyValue::String("desktop".to_owned()),
        );
        value.validate(&limits).expect("valid entry");

        let mut too_long_id = value.clone();
        too_long_id.id = PinnedMemoryId::new("memory_identifier_too_long").expect("valid text");
        assert!(matches!(
            too_long_id.validate(&limits),
            Err(PinnedMemoryValidationError::TooLong {
                field: "pinned memory id",
                ..
            })
        ));

        let mut blank_content = value.clone();
        blank_content.content = " \n ".to_owned();
        assert!(matches!(
            blank_content.validate(&limits),
            Err(PinnedMemoryValidationError::Blank {
                field: "pinned memory content"
            })
        ));

        let mut bad_attribute = value.clone();
        bad_attribute.attributes.insert(
            "bad\0key".to_owned(),
            MemoryPropertyValue::String("value".to_owned()),
        );
        assert!(matches!(
            bad_attribute.validate(&limits),
            Err(PinnedMemoryValidationError::ControlCharacter {
                field: "pinned memory attribute key"
            })
        ));

        let mut too_long_category = value.clone();
        too_long_category.category =
            PinnedMemoryCategory::new("category_name_too_long").expect("valid text");
        assert!(matches!(
            too_long_category.validate(&limits),
            Err(PinnedMemoryValidationError::TooLong {
                field: "pinned memory category",
                ..
            })
        ));

        let mut too_long_content = value.clone();
        too_long_content.content = "内容".repeat(22);
        assert!(matches!(
            too_long_content.validate(&limits),
            Err(PinnedMemoryValidationError::TooLong {
                field: "pinned memory content",
                actual_bytes: 132,
                ..
            })
        ));

        let mut too_many_attributes = value.clone();
        too_many_attributes.attributes.insert(
            "source".to_owned(),
            MemoryPropertyValue::String("user".to_owned()),
        );
        too_many_attributes.attributes.insert(
            "topic".to_owned(),
            MemoryPropertyValue::String("ui".to_owned()),
        );
        assert!(matches!(
            too_many_attributes.validate(&limits),
            Err(PinnedMemoryValidationError::TooManyAttributes { actual: 3, max: 2 })
        ));

        let mut too_long_key = value.clone();
        too_long_key.attributes = BTreeMap::from([(
            "attribute_key_too_long".to_owned(),
            MemoryPropertyValue::String("value".to_owned()),
        )]);
        assert!(matches!(
            too_long_key.validate(&limits),
            Err(PinnedMemoryValidationError::TooLong {
                field: "pinned memory attribute key",
                ..
            })
        ));

        let mut too_long_string = value.clone();
        too_long_string.attributes = BTreeMap::from([(
            "scope".to_owned(),
            MemoryPropertyValue::String("x".repeat(33)),
        )]);
        assert!(matches!(
            too_long_string.validate(&limits),
            Err(PinnedMemoryValidationError::TooLong {
                field: "pinned memory attribute string",
                ..
            })
        ));

        let mut bad_string = value.clone();
        bad_string.attributes = BTreeMap::from([(
            "scope".to_owned(),
            MemoryPropertyValue::String("bad\0value".to_owned()),
        )]);
        assert!(matches!(
            bad_string.validate(&limits),
            Err(PinnedMemoryValidationError::ControlCharacter {
                field: "pinned memory attribute string"
            })
        ));

        let mut multiline = value;
        multiline.content = "line 1\nline 2\tvalue".to_owned();
        multiline.attributes = BTreeMap::from([(
            "note".to_owned(),
            MemoryPropertyValue::String("line 1\nline 2".to_owned()),
        )]);
        multiline
            .validate(&limits)
            .expect("XML whitespace remains valid content");
    }

    #[test]
    fn pinned_draft_has_no_id_and_patch_distinguishes_clear_from_absent() {
        let limits = limits();
        let draft = PinnedMemoryDraft {
            category: PinnedMemoryCategory::new("preference").expect("valid category"),
            content: "Use dark mode".to_owned(),
            attributes: BTreeMap::new(),
        };
        draft.validate(&limits).expect("valid draft");

        assert_eq!(
            PinnedMemoryPatch::default().validate(&limits),
            Err(PinnedMemoryValidationError::EmptyPatch)
        );
        let clear = PinnedMemoryPatch {
            attributes: Some(BTreeMap::new()),
            ..PinnedMemoryPatch::default()
        };
        assert!(!clear.is_empty());
        clear.validate(&limits).expect("explicit clear is valid");
    }
}
