//! Conversation 文件的记录边界。旧纯消息只供迁移源读取，不能与新历史帧混写。

use std::io::{BufRead, Write};

use agent_types::ConversationMessage;
use assistant_protocol::ConversationOwner;
use assistant_runtime::StoredHistoryFacts;
use serde::{Deserialize, Serialize};

use super::super::{StorageResult, internal_error, invalid_data, invalid_data_with_source};

pub(super) const HISTORY_FORMAT: &str = "ez_assistant_history";
pub(super) const HISTORY_VERSION: u32 = 1;
const GENERATION_BATCH_SIZE: usize = 64;

/// 新 generation 的消息与检查点按有界批次写入；工具配对闭包不因批次上限被拆开。
pub(super) fn write_generation(
    writer: &mut impl Write,
    owner: &ConversationOwner,
    generation: u64,
    messages: impl IntoIterator<Item = StorageResult<ConversationMessage>>,
    history: &StoredHistoryFacts,
) -> StorageResult<()> {
    write_record(
        writer,
        &HistoryRecord::Header(HistoryHeader {
            format: HISTORY_FORMAT.to_owned(),
            version: HISTORY_VERSION,
            owner: owner.clone(),
            generation,
        }),
    )?;
    // 多个物理批次属于同一 generation 切换，不为它们分配新的业务提交身份。
    let operation_id = format!("generation:{generation}");
    let mut validation = agent_types::ToolExchangeValidator::default();
    let mut batch = Vec::with_capacity(GENERATION_BATCH_SIZE);
    for message in messages {
        let message = message?;
        validation.push(&message).map_err(|source| {
            invalid_data_with_source("replacement history tool exchange is invalid", source)
        })?;
        batch.push(message);
        if validation.is_complete() && batch.len() >= GENERATION_BATCH_SIZE {
            write_record(
                writer,
                &HistoryRecord::Commit(HistoryFrame {
                    operation_id: operation_id.clone(),
                    messages: std::mem::take(&mut batch),
                    history: StoredHistoryFacts::default(),
                }),
            )?;
        }
    }
    validation.finish().map_err(|source| {
        invalid_data_with_source("replacement history splits a tool exchange", source)
    })?;
    if !batch.is_empty() {
        write_record(
            writer,
            &HistoryRecord::Commit(HistoryFrame {
                operation_id: operation_id.clone(),
                messages: batch,
                history: StoredHistoryFacts::default(),
            }),
        )?;
    }
    macro_rules! facts {
        ($field:ident) => {
            for chunk in history.$field.chunks(GENERATION_BATCH_SIZE) {
                write_record(
                    writer,
                    &HistoryRecord::Commit(HistoryFrame {
                        operation_id: operation_id.clone(),
                        messages: Vec::new(),
                        history: StoredHistoryFacts {
                            $field: chunk.to_vec(),
                            ..Default::default()
                        },
                    }),
                )?;
            }
        };
    }
    facts!(inputs);
    facts!(runs);
    facts!(message_origins);
    facts!(child_tasks);
    facts!(commands);
    facts!(skill_activations);
    facts!(mcp_selections);
    facts!(feedback);
    facts!(questions);
    facts!(attachments);
    Ok(())
}

/// 一次追加沿用既有 operation_id；payload 只暂存于未完成的跨介质提交回执。
pub(super) fn encode_append(
    operation_id: &str,
    messages: &[ConversationMessage],
    history: StoredHistoryFacts,
) -> StorageResult<Vec<u8>> {
    let frame = validate_frame(HistoryFrame {
        operation_id: operation_id.to_owned(),
        messages: messages.to_vec(),
        history,
    })?;
    let mut bytes = Vec::new();
    write_record(&mut bytes, &HistoryRecord::Commit(frame))?;
    Ok(bytes)
}

fn write_record(writer: &mut impl Write, record: &HistoryRecord) -> StorageResult<()> {
    serde_json::to_writer(&mut *writer, record)
        .map_err(|source| internal_error("conversation history could not be encoded", source))?;
    writer.write_all(b"\n").map_err(|source| {
        internal_error("conversation history record could not be written", source)
    })
}

/// 头只绑定格式与物理 owner；不是每次提交都会变化的业务 revision。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HistoryHeader {
    pub format: String,
    pub version: u32,
    pub owner: ConversationOwner,
    pub generation: u64,
}

/// 一次 staged append 对应一帧。状态变化可以没有消息，但不接受全空帧。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HistoryFrame {
    pub operation_id: String,
    pub messages: Vec<ConversationMessage>,
    #[serde(default)]
    pub history: StoredHistoryFacts,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum HistoryRecord {
    Header(HistoryHeader),
    Commit(HistoryFrame),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Encoding {
    Legacy,
    History,
}

pub(super) struct RecordReader<R> {
    reader: R,
    encoding: Option<Encoding>,
    pub(super) header: Option<HistoryHeader>,
    line: Vec<u8>,
    offset: u64,
}

pub(super) struct Batch {
    pub offset: u64,
    pub messages: Vec<ConversationMessage>,
    pub history: StoredHistoryFacts,
}

impl<R: BufRead> RecordReader<R> {
    pub(super) fn new(reader: R) -> Self {
        Self {
            reader,
            encoding: None,
            header: None,
            line: Vec::new(),
            offset: 0,
        }
    }

    pub(super) fn encoding(&self) -> Encoding {
        self.encoding.unwrap_or(Encoding::Legacy)
    }

    pub(super) fn next(&mut self) -> StorageResult<Option<Batch>> {
        loop {
            let start = self.offset;
            self.line.clear();
            let read = self
                .reader
                .read_until(b'\n', &mut self.line)
                .map_err(|source| {
                    internal_error("conversation record could not be read", source)
                })?;
            if read == 0 {
                return Ok(None);
            }
            self.offset = self
                .offset
                .checked_add(read as u64)
                .ok_or_else(|| invalid_data("conversation byte offset exceeds storage range"))?;
            let line = complete_line(&self.line)?;
            if self.encoding == Some(Encoding::Legacy) {
                return decode_legacy(line, start).map(Some);
            }
            // 迁移前的文件没有头；只在第一行允许选择旧格式，之后绝不自动回退。
            if self.encoding.is_none() {
                let value: serde_json::Value = serde_json::from_slice(line).map_err(|source| {
                    invalid_data_with_source("conversation JSONL is invalid", source)
                })?;
                if value.get("role").is_some() {
                    self.encoding = Some(Encoding::Legacy);
                    return decode_legacy(line, start).map(Some);
                }
            }
            // 格式已由头确定，正文直接解码为契约类型，避免先复制整帧为通用 JSON 树。
            let record: HistoryRecord = serde_json::from_slice(line).map_err(|source| {
                invalid_data_with_source("conversation history record is invalid", source)
            })?;
            match record {
                HistoryRecord::Header(header) => {
                    if self.encoding.is_some()
                        || header.format != HISTORY_FORMAT
                        || header.version != HISTORY_VERSION
                        || header.generation == 0
                    {
                        return Err(invalid_data("conversation history header is invalid"));
                    }
                    self.encoding = Some(Encoding::History);
                    self.header = Some(header);
                }
                HistoryRecord::Commit(frame) => {
                    if self.encoding != Some(Encoding::History) {
                        return Err(invalid_data("conversation history header is missing"));
                    }
                    let frame = validate_frame(frame)?;
                    if let Some(header) = &self.header {
                        validate_frame_owner(&frame, &header.owner)?;
                    }
                    return Ok(Some(Batch {
                        offset: start,
                        messages: frame.messages,
                        history: frame.history,
                    }));
                }
            }
        }
    }
}

/// 只在已经完整核验的 offset 索引定位后读取一条记录；不把片段当完整文件猜格式。
pub(super) fn decode_indexed_record(
    bytes: &[u8],
    encoding: Encoding,
) -> StorageResult<Vec<ConversationMessage>> {
    let line = complete_line(bytes)?;
    match encoding {
        Encoding::Legacy => Ok(decode_legacy(line, 0)?.messages),
        Encoding::History => Ok(decode_commit(bytes)?.messages),
    }
}

pub(super) fn decode_commit(bytes: &[u8]) -> StorageResult<HistoryFrame> {
    let record: HistoryRecord =
        serde_json::from_slice(complete_line(bytes)?).map_err(|source| {
            invalid_data_with_source("conversation history record is invalid", source)
        })?;
    let HistoryRecord::Commit(frame) = record else {
        return Err(invalid_data(
            "conversation index points to a history header",
        ));
    };
    validate_frame(frame)
}

fn complete_line(bytes: &[u8]) -> StorageResult<&[u8]> {
    if bytes.last() != Some(&b'\n') {
        return Err(invalid_data(
            "conversation contains an incomplete JSONL record",
        ));
    }
    let line = &bytes[..bytes.len() - 1];
    if line.is_empty() {
        return Err(invalid_data("conversation contains an empty JSONL record"));
    }
    Ok(line)
}

fn decode_legacy(line: &[u8], offset: u64) -> StorageResult<Batch> {
    // serde 的未知字段默认忽略不能允许伪装为旧消息的帧跨越格式边界。
    let value: serde_json::Value = serde_json::from_slice(line)
        .map_err(|source| invalid_data_with_source("conversation JSONL is invalid", source))?;
    if value.get("type").is_some() || value.get("format").is_some() {
        return Err(invalid_data(
            "conversation contains mixed history encodings",
        ));
    }
    let message = serde_json::from_value(value)
        .map_err(|source| invalid_data_with_source("conversation JSONL is invalid", source))?;
    Ok(Batch {
        offset,
        messages: vec![message],
        history: StoredHistoryFacts::default(),
    })
}

fn validate_frame(mut frame: HistoryFrame) -> StorageResult<HistoryFrame> {
    if frame.operation_id.trim().is_empty()
        || (frame.messages.is_empty() && frame.history.is_empty())
        || frame.history.runs.iter().any(|run| {
            run.attempt == 0
                || (run.partial
                    && (run.status.is_some()
                        || run.finished_at_ms.is_some()
                        || run.error.is_some()))
        })
        || frame
            .history
            .message_origins
            .iter()
            .any(|origin| origin.step == Some(0) || origin.attempt == Some(0))
        || frame
            .history
            .commands
            .iter()
            .any(|command| command.state != assistant_runtime::StoredSessionCommandState::Committed)
        || frame
            .history
            .child_tasks
            .iter()
            .any(|child| child.body_generation == 0)
        || frame
            .history
            .questions
            .iter()
            .any(|question| question.status == assistant_protocol::FeedbackStatus::Pending)
    {
        return Err(invalid_data("conversation history commit is invalid"));
    }
    let conversation = agent_types::ConversationSnapshot::new(std::mem::take(&mut frame.messages));
    conversation
        .validate_tool_exchange_pairs()
        .map_err(|source| {
            invalid_data_with_source("conversation history frame splits a tool exchange", source)
        })?;
    frame.messages = conversation.messages;
    Ok(frame)
}

pub(super) fn validate_frame_owner(
    frame: &HistoryFrame,
    owner: &ConversationOwner,
) -> StorageResult<()> {
    for activation in &frame.history.skill_activations {
        super::super::skill::validate_activation(activation)?;
    }
    let session = match owner {
        ConversationOwner::MainSession { session_id }
        | ConversationOwner::ChildTask { session_id, .. } => session_id,
    };
    let wrong_activation = frame.history.skill_activations.iter().any(|activation| {
        &activation.session_id != session
            || !match (&activation.owner, owner) {
                (
                    assistant_runtime::SkillActivationOwner::Session(id),
                    ConversationOwner::MainSession { session_id },
                ) => id == session_id,
                (
                    assistant_runtime::SkillActivationOwner::ChildTask(id),
                    ConversationOwner::ChildTask { child_task_id, .. },
                ) => id == child_task_id.as_str(),
                _ => false,
            }
    });
    if wrong_activation
        || frame
            .history
            .commands
            .iter()
            .any(|command| &command.session_id != session)
        || frame
            .history
            .mcp_selections
            .iter()
            .any(|selection| &selection.session_id != session)
        || frame
            .history
            .questions
            .iter()
            .any(|question| &question.session_id != session)
    {
        return Err(invalid_data(
            "conversation history fact belongs to a different owner",
        ));
    }
    for attachment in &frame.history.attachments {
        let path = &attachment.relative_path;
        if path.is_empty()
            || path.starts_with(['/', '\\'])
            || path.split(['/', '\\']).any(|part| part == "..")
            || (path.as_bytes().get(1) == Some(&b':'))
        {
            return Err(invalid_data(
                "conversation history attachment path escapes its owner",
            ));
        }
    }
    Ok(())
}
