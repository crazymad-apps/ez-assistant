use quick_xml::{
    Writer,
    events::{BytesEnd, BytesStart, BytesText, Event},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    PinnedMemoryEntry, PinnedMemoryLimits, PinnedMemoryValidationError,
    entry::validate_snapshot_input,
};

/// 渲染 Pinned Memory System Prompt Part 所需的结构化输入。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedMemorySnapshotInput {
    /// 告诉模型如何理解和使用这些常驻记忆的整体说明。
    pub description: String,
    /// 当前 Store 中需要冻结进新会话的完整条目。
    pub entries: Vec<PinnedMemoryEntry>,
}

/// 已完成验证和确定性渲染的 Pinned Memory System Prompt Part。
///
/// 该类型只保存最终 XML，不提供反向解析回领域条目的接口。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PinnedMemorySnapshot {
    content: String,
}

impl PinnedMemorySnapshot {
    /// 校验结构化输入并生成固定 XML 结构。
    pub fn render(
        input: PinnedMemorySnapshotInput,
        limits: &PinnedMemoryLimits,
    ) -> Result<Self, PinnedMemorySnapshotError> {
        validate_snapshot_input(&input.description, &input.entries, limits)?;

        let mut entries: Vec<&PinnedMemoryEntry> = input.entries.iter().collect();
        entries.sort_by(|left, right| {
            (left.category.as_str(), left.id.as_str())
                .cmp(&(right.category.as_str(), right.id.as_str()))
        });

        let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);
        writer.write_event(Event::Start(BytesStart::new("pinned_memories")))?;
        write_text_element(&mut writer, "description", &input.description)?;
        writer.write_event(Event::Start(BytesStart::new("entries")))?;
        for entry in entries {
            write_entry(&mut writer, entry)?;
        }
        writer.write_event(Event::End(BytesEnd::new("entries")))?;
        writer.write_event(Event::End(BytesEnd::new("pinned_memories")))?;

        let bytes = writer.into_inner();
        if bytes.len() > limits.max_snapshot_bytes.get() {
            return Err(PinnedMemorySnapshotError::CapacityExceeded {
                actual_bytes: bytes.len(),
                max_bytes: limits.max_snapshot_bytes.get(),
            });
        }
        let content =
            String::from_utf8(bytes).map_err(|_| PinnedMemorySnapshotError::InvalidUtf8Output)?;
        Ok(Self { content })
    }

    /// 借用最终 XML 内容。
    pub fn content(&self) -> &str {
        &self.content
    }

    /// 消费快照并取回最终 XML 内容。
    pub fn into_content(self) -> String {
        self.content
    }
}

/// Pinned Memory 快照无法生成。
#[derive(Debug, Error)]
pub enum PinnedMemorySnapshotError {
    /// 结构化输入违反领域或容量不变量。
    #[error(transparent)]
    Validation(#[from] PinnedMemoryValidationError),
    /// XML Writer 无法完成内存输出。
    #[error("failed to render pinned memory XML: {0}")]
    Render(#[from] std::io::Error),
    /// 最终 XML 超过显式快照字节上限。
    #[error("pinned memory snapshot is {actual_bytes} bytes, exceeding the {max_bytes} byte limit")]
    CapacityExceeded {
        /// 实际 XML UTF-8 字节数。
        actual_bytes: usize,
        /// 允许的最大 XML UTF-8 字节数。
        max_bytes: usize,
    },
    /// Writer 违反了 UTF-8 输出不变量。
    #[error("pinned memory XML writer produced invalid UTF-8")]
    InvalidUtf8Output,
}

fn write_entry(
    writer: &mut Writer<Vec<u8>>,
    entry: &PinnedMemoryEntry,
) -> Result<(), std::io::Error> {
    let mut memory = BytesStart::new("memory");
    memory.push_attribute(("id", entry.id.as_str()));
    memory.push_attribute(("category", entry.category.as_str()));
    writer.write_event(Event::Start(memory))?;

    writer.write_event(Event::Start(BytesStart::new("properties")))?;
    for (name, value) in &entry.attributes {
        let mut property = BytesStart::new("property");
        property.push_attribute(("name", name.as_str()));
        property.push_attribute(("type", value.kind()));
        writer.write_event(Event::Start(property))?;
        writer.write_event(Event::Text(BytesText::new(&value.text())))?;
        writer.write_event(Event::End(BytesEnd::new("property")))?;
    }
    writer.write_event(Event::End(BytesEnd::new("properties")))?;
    write_text_element(writer, "content", &entry.content)?;
    writer.write_event(Event::End(BytesEnd::new("memory")))?;
    Ok(())
}

fn write_text_element(
    writer: &mut Writer<Vec<u8>>,
    name: &str,
    content: &str,
) -> Result<(), std::io::Error> {
    writer.write_event(Event::Start(BytesStart::new(name)))?;
    writer.write_event(Event::Text(BytesText::new(content)))?;
    writer.write_event(Event::End(BytesEnd::new(name)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, num::NonZeroUsize};

    use crate::{MemoryPropertyValue, PinnedMemoryCategory, PinnedMemoryId};

    use super::*;
    use crate::pinned::entry::tests::{entry, limits};

    #[test]
    fn pinned_snapshot_is_deterministic_sorted_and_safely_escaped() {
        let mut second = entry("memory_2", "zeta", "第二条 <内容>");
        second.attributes = BTreeMap::from([
            (
                "quote\"<&".to_owned(),
                MemoryPropertyValue::String("value ' \" < & >".to_owned()),
            ),
            (
                "rank".to_owned(),
                MemoryPropertyValue::Number(serde_json::Number::from(2)),
            ),
        ]);
        let first = entry("memory_1", "alpha", "第一条");
        let description = "Use <pinned> & trusted \"facts\".".to_owned();

        let forward = PinnedMemorySnapshot::render(
            PinnedMemorySnapshotInput {
                description: description.clone(),
                entries: vec![second.clone(), first.clone()],
            },
            &limits(),
        )
        .expect("render forward input");
        let reverse = PinnedMemorySnapshot::render(
            PinnedMemorySnapshotInput {
                description,
                entries: vec![first, second],
            },
            &limits(),
        )
        .expect("render reverse input");

        assert_eq!(forward, reverse);
        assert_eq!(
            forward.content(),
            "<pinned_memories>\n  <description>Use &lt;pinned&gt; &amp; trusted &quot;facts&quot;.</description>\n  <entries>\n    <memory id=\"memory_1\" category=\"alpha\">\n      <properties>\n      </properties>\n      <content>第一条</content>\n    </memory>\n    <memory id=\"memory_2\" category=\"zeta\">\n      <properties>\n        <property name=\"quote&quot;&lt;&amp;\" type=\"string\">value &apos; &quot; &lt; &amp; &gt;</property>\n        <property name=\"rank\" type=\"number\">2</property>\n      </properties>\n      <content>第二条 &lt;内容&gt;</content>\n    </memory>\n  </entries>\n</pinned_memories>"
        );
    }

    #[test]
    fn pinned_snapshot_keeps_empty_containers_and_round_trips_as_text() {
        let snapshot = PinnedMemorySnapshot::render(
            PinnedMemorySnapshotInput {
                description: "No pinned memories are currently configured.".to_owned(),
                entries: vec![],
            },
            &limits(),
        )
        .expect("render empty snapshot");
        assert_eq!(
            snapshot.content(),
            "<pinned_memories>\n  <description>No pinned memories are currently configured.</description>\n  <entries>\n  </entries>\n</pinned_memories>"
        );
        let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
        assert_eq!(
            serde_json::from_str::<PinnedMemorySnapshot>(&json).expect("deserialize snapshot"),
            snapshot
        );
    }

    #[test]
    fn pinned_snapshot_rejects_entry_description_and_final_size_limits() {
        let mut small = limits();
        small.max_entries = NonZeroUsize::new(1).expect("non-zero");
        assert!(matches!(
            PinnedMemorySnapshot::render(
                PinnedMemorySnapshotInput {
                    description: "description".to_owned(),
                    entries: vec![entry("a", "alpha", "one"), entry("b", "beta", "two")],
                },
                &small,
            ),
            Err(PinnedMemorySnapshotError::Validation(
                PinnedMemoryValidationError::TooManyEntries { .. }
            ))
        ));

        let mut small = limits();
        small.max_description_bytes = NonZeroUsize::new(4).expect("non-zero");
        assert!(matches!(
            PinnedMemorySnapshot::render(
                PinnedMemorySnapshotInput {
                    description: "description".to_owned(),
                    entries: vec![],
                },
                &small,
            ),
            Err(PinnedMemorySnapshotError::Validation(
                PinnedMemoryValidationError::TooLong {
                    field: "pinned memory description",
                    ..
                }
            ))
        ));

        let mut small = limits();
        small.max_snapshot_bytes = NonZeroUsize::new(32).expect("non-zero");
        assert!(matches!(
            PinnedMemorySnapshot::render(
                PinnedMemorySnapshotInput {
                    description: "description".to_owned(),
                    entries: vec![],
                },
                &small,
            ),
            Err(PinnedMemorySnapshotError::CapacityExceeded { .. })
        ));
    }

    #[test]
    fn pinned_snapshot_validates_deserialized_entries_before_rendering() {
        let entry = PinnedMemoryEntry {
            id: PinnedMemoryId::new("memory_1").expect("valid id"),
            category: PinnedMemoryCategory::new("preference").expect("valid category"),
            content: "bad\0content".to_owned(),
            attributes: BTreeMap::new(),
        };
        assert!(matches!(
            PinnedMemorySnapshot::render(
                PinnedMemorySnapshotInput {
                    description: "description".to_owned(),
                    entries: vec![entry],
                },
                &limits(),
            ),
            Err(PinnedMemorySnapshotError::Validation(
                PinnedMemoryValidationError::ControlCharacter {
                    field: "pinned memory content"
                }
            ))
        ));
    }
}
