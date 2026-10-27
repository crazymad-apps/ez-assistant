//! 规范 Conversation 的 JSONL 编解码和文件提交原语。

mod codec;
mod history_index;
mod index_append;
mod stream;
#[cfg(test)]
mod tests;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use agent_types::{ConversationMessage, ConversationSnapshot, MessageId};
use assistant_runtime::StoredHistoryFacts;

use super::{StorageResult, internal_error, invalid_data};
use codec::{Encoding, HistoryHeader, RecordReader};
pub(super) use stream::{scan_document, write_upgraded_generation};

const INDEX_CACHE_CAPACITY: usize = 32;

/// 同一可靠文件的正文与历史；无头旧文件仅作为迁移源，不能据空历史猜测其格式。
pub(super) struct ConversationDocument {
    pub conversation: ConversationSnapshot,
    pub history: Option<StoredHistoryFacts>,
}

/// 同一提交帧可以包含多条消息，字节位置与帧内序号必须共同定位。
#[derive(Clone, Copy)]
struct MessagePosition {
    offset: u64,
    index: usize,
}

struct CachedConversationIndex {
    path: PathBuf,
    file_length: u64,
    modified: std::time::SystemTime,
    offsets: ConversationOffsets,
}

#[derive(Clone)]
struct ConversationOffsets {
    message: Arc<[MessagePosition]>,
    // 可见消息映射到物理消息序号；元数据帧从不增加任何消息序号。
    display: Arc<[usize]>,
    encoding: Encoding,
    header: Option<HistoryHeader>,
    history: Arc<history_index::HistoryOffsets>,
    locations: Arc<HashMap<MessageId, usize>>,
    // 完整交换边界的调用 ID 校验状态；不保留消息、工具结果或历史事实。
    validation: Arc<agent_types::ToolExchangeValidator>,
}

/// Store Worker 私有的可重建 LRU；缓存位置和最新元数据，不缓存 Conversation 正文。
pub(super) struct ConversationIndexCache {
    entries: VecDeque<CachedConversationIndex>,
}

impl Default for ConversationIndexCache {
    fn default() -> Self {
        Self {
            entries: VecDeque::with_capacity(INDEX_CACHE_CAPACITY),
        }
    }
}

impl ConversationIndexCache {
    /// 完整正文读取同时构建同一文件的位置索引，冷加载的后续历史／分页查询直接复用。
    /// 只将位置和校验 ID 留在 LRU；正文所有权移交调用方，不在 Store 缓存第二份正文。
    pub(super) fn read_owned(
        &mut self,
        path: &Path,
        owner: &assistant_protocol::ConversationOwner,
        generation: u64,
    ) -> StorageResult<ConversationSnapshot> {
        self.entries.retain(|entry| entry.path != path);
        let metadata = path
            .metadata()
            .map_err(|source| internal_error("conversation metadata could not be read", source))?;
        let modified = metadata.modified().map_err(|source| {
            internal_error("conversation modification time could not be read", source)
        })?;
        let mut messages = Vec::new();
        let offsets = build_offsets(
            path,
            Some(&mut |message| {
                messages.push(message);
                Ok(())
            }),
        )?;
        if offsets
            .header
            .as_ref()
            .is_none_or(|header| &header.owner != owner || header.generation != generation)
        {
            return Err(invalid_data(
                "conversation history owner or generation does not match",
            ));
        }
        if self.entries.len() == INDEX_CACHE_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(CachedConversationIndex {
            path: path.to_path_buf(),
            file_length: metadata.len(),
            modified,
            offsets,
        });
        Ok(ConversationSnapshot::new(messages))
    }

    /// 扫描时立即释放被可靠压缩点覆盖的正文，同时构建唯一位置索引。
    /// 返回前完成 owner、generation、完整工具交换和历史引用校验；不运行恢复业务或图片解码。
    pub(super) fn read_execution_window(
        &mut self,
        path: &Path,
        owner: &assistant_protocol::ConversationOwner,
        generation: u64,
    ) -> StorageResult<assistant_runtime::ExecutionWindow> {
        let metadata = path
            .metadata()
            .map_err(|e| internal_error("conversation metadata unavailable", e))?;
        let modified = metadata
            .modified()
            .map_err(|e| internal_error("conversation timestamp unavailable", e))?;
        let mut window = assistant_runtime::ExecutionWindow::default();
        let offsets = build_offsets(path, Some(&mut |message| window.push(message)))?;
        if offsets
            .header
            .as_ref()
            .is_none_or(|header| &header.owner != owner || header.generation != generation)
        {
            return Err(invalid_data(
                "execution window owner or generation does not match",
            ));
        }
        window.finish()?;
        self.entries.retain(|entry| entry.path != path);
        if self.entries.len() == INDEX_CACHE_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(CachedConversationIndex {
            path: path.to_path_buf(),
            file_length: metadata.len(),
            modified,
            offsets,
        });
        Ok(window)
    }

    /// 复用已核对 owner 的唯一键；不把已结束控制重新放回 SQL 以维持身份保护。
    pub(super) fn input_identity_conflicts(
        &mut self,
        path: &Path,
        owner: &assistant_protocol::ConversationOwner,
        generation: u64,
        input_id: &assistant_protocol::InputId,
        run_id: Option<&assistant_protocol::RunId>,
        message_id: &MessageId,
    ) -> StorageResult<bool> {
        self.validate_owner(path, owner, generation)?;
        let offsets = self.offsets(path)?;
        Ok(offsets.history.contains_input(input_id)
            || run_id.is_some_and(|run| offsets.history.contains_run(run))
            || offsets.locations.contains_key(message_id))
    }

    /// 文件身份及完整结构通过校验后，仅以同一位置索引的唯一键投影删除影响。
    /// 未完成控制与上传由 Store 提供；历史键只参与计数，不恢复 Run／child 或产生副作用。
    pub(super) fn deletion_impact(
        &mut self,
        path: &Path,
        owner: &assistant_protocol::ConversationOwner,
        generation: u64,
        runs: &HashSet<assistant_protocol::RunId>,
        children: &HashSet<assistant_protocol::ChildTaskId>,
        attachments: &HashSet<assistant_protocol::AttachmentId>,
    ) -> StorageResult<assistant_protocol::DeleteSessionImpact> {
        self.validate_owner(path, owner, generation)?;
        let offsets = self.offsets(path)?;
        let (run_count, child_task_count, attachment_count) =
            offsets.history.deletion_counts(runs, children, attachments);
        Ok(assistant_protocol::DeleteSessionImpact {
            message_count: u64::try_from(offsets.message.len())
                .map_err(|_| invalid_data("delete message count is invalid"))?,
            run_count: u64::try_from(run_count)
                .map_err(|_| invalid_data("delete run count is invalid"))?,
            child_task_count: u64::try_from(child_task_count)
                .map_err(|_| invalid_data("delete child count is invalid"))?,
            attachment_count: u64::try_from(attachment_count)
                .map_err(|_| invalid_data("delete attachment count is invalid"))?,
        })
    }

    pub(super) fn read_child_history(
        &mut self,
        path: &Path,
        owner: &assistant_protocol::ConversationOwner,
        generation: u64,
        child_task_id: Option<&assistant_protocol::ChildTaskId>,
    ) -> StorageResult<Vec<assistant_runtime::StoredChildTaskHistory>> {
        let offsets = self.offsets(path)?;
        let header = offsets
            .header
            .as_ref()
            .ok_or_else(|| invalid_data("conversation history header is missing"))?;
        if &header.owner != owner || header.generation != generation {
            return Err(invalid_data(
                "conversation history owner or generation does not match",
            ));
        }
        Ok(offsets.history.children(child_task_id))
    }

    pub(super) fn read_run_origins(
        &mut self,
        path: &Path,
        owner: &assistant_protocol::ConversationOwner,
        generation: u64,
        runs: &HashSet<assistant_protocol::RunId>,
    ) -> StorageResult<Vec<assistant_runtime::StoredMessageOrigin>> {
        let offsets = self.offsets(path)?;
        let header = offsets
            .header
            .as_ref()
            .ok_or_else(|| invalid_data("conversation history header is missing"))?;
        if &header.owner != owner || header.generation != generation {
            return Err(invalid_data(
                "conversation history owner or generation does not match",
            ));
        }
        let mut origins = offsets.history.run_origins(runs);
        origins.sort_by_key(|origin| {
            offsets
                .locations
                .get(&origin.message_id)
                .copied()
                .unwrap_or(usize::MAX)
        });
        Ok(origins)
    }
    pub(super) fn read_history(
        &mut self,
        path: &Path,
        owner: &assistant_protocol::ConversationOwner,
        generation: u64,
    ) -> StorageResult<assistant_runtime::StoredConversationHistory> {
        let offsets = self.offsets(path)?;
        let header = offsets
            .header
            .as_ref()
            .ok_or_else(|| invalid_data("conversation history header is missing"))?;
        if &header.owner != owner || header.generation != generation {
            return Err(invalid_data(
                "conversation history owner or generation does not match",
            ));
        }
        Ok(assistant_runtime::StoredConversationHistory {
            generation,
            facts: offsets.history.snapshot(),
        })
    }
    pub(super) fn read_input_history(
        &mut self,
        path: &Path,
        owner: &assistant_protocol::ConversationOwner,
        generation: u64,
        input_id: &assistant_protocol::InputId,
    ) -> StorageResult<Option<assistant_runtime::StoredInputHistory>> {
        self.validate_owner(path, owner, generation)?;
        Ok(self.offsets(path)?.history.input(input_id))
    }
    pub(super) fn read_history_window(
        &mut self,
        path: &Path,
        conversation: &ConversationSnapshot,
    ) -> StorageResult<Option<StoredHistoryFacts>> {
        let offsets = self.offsets(path)?;
        Ok(offsets
            .header
            .as_ref()
            .map(|header| offsets.history.window(header, conversation)))
    }

    /// 路径由 Store 选择后仍核对文件内的 owner，防止错置文件被当成另一个会话读取。
    /// 旧无头文件仅在迁移切换前存在；新格式不得通过路径推断来覆盖头内身份。
    pub(super) fn validate_owner(
        &mut self,
        path: &Path,
        owner: &assistant_protocol::ConversationOwner,
        generation: u64,
    ) -> StorageResult<()> {
        let offsets = self.offsets(path)?;
        let header = offsets
            .header
            .as_ref()
            .ok_or_else(|| invalid_data("conversation history header is missing"))?;
        if &header.owner != owner || header.generation != generation {
            return Err(invalid_data(
                "conversation history owner or generation does not match",
            ));
        }
        Ok(())
    }

    pub(super) fn remove_under(&mut self, directory: &Path) {
        self.entries
            .retain(|entry| !entry.path.starts_with(directory));
    }

    pub(super) fn read_window(
        &mut self,
        path: &Path,
        generation: u64,
        requested_end: Option<usize>,
        limit: usize,
    ) -> StorageResult<assistant_runtime::StoredConversationWindow> {
        let offsets = self.offsets(path)?;
        if offsets
            .header
            .as_ref()
            .is_some_and(|header| header.generation != generation)
        {
            return Err(invalid_data(
                "conversation history generation does not match",
            ));
        }
        let total = offsets.display.len();
        let end = requested_end.unwrap_or(total).min(total);
        let start = end.saturating_sub(limit);
        let raw_start = offsets
            .display
            .get(start)
            .copied()
            .unwrap_or(offsets.message.len());
        let raw_end = offsets
            .display
            .get(end)
            .copied()
            .unwrap_or(offsets.message.len());
        let conversation =
            read_positions(path, &offsets.message[raw_start..raw_end], offsets.encoding)?;
        let history = offsets
            .header
            .as_ref()
            .map(|header| offsets.history.window(header, &conversation));
        Ok(assistant_runtime::StoredConversationWindow {
            generation,
            start,
            end,
            total,
            conversation,
            history,
        })
    }

    pub(super) fn read_raw_window(
        &mut self,
        path: &Path,
        start: usize,
        limit: usize,
    ) -> StorageResult<(ConversationSnapshot, usize, usize)> {
        let offsets = self.offsets(path)?;
        let total = offsets.message.len();
        let start = start.min(total);
        let end = start.saturating_add(limit).min(total);
        Ok((
            read_positions(path, &offsets.message[start..end], offsets.encoding)?,
            end,
            total,
        ))
    }

    /// 同代可靠正文的末位置用于冻结无输出终结边界；只读已校验的位置键，不解码正文。
    pub(super) fn last_message_id(
        &mut self,
        path: &Path,
        owner: &assistant_protocol::ConversationOwner,
        generation: u64,
    ) -> StorageResult<Option<MessageId>> {
        self.validate_owner(path, owner, generation)?;
        // 末条可能是 Tool；不能把它单独构造成必须闭合工具交换的 ConversationSnapshot。
        Ok(self
            .offsets(path)?
            .locations
            .iter()
            .max_by_key(|(_, ordinal)| *ordinal)
            .map(|(id, _)| id.clone()))
    }

    /// 使用同一帧索引定位，避免每条帧内消息都重读整个提交帧。
    pub(super) fn locate_message(
        &mut self,
        path: &Path,
        target: &MessageId,
    ) -> StorageResult<Option<(usize, Option<usize>)>> {
        let offsets = self.offsets(path)?;
        Ok(offsets
            .locations
            .get(target)
            .map(|ordinal| (*ordinal, offsets.display.binary_search(ordinal).ok())))
    }

    fn offsets(&mut self, path: &Path) -> StorageResult<ConversationOffsets> {
        let metadata = path
            .metadata()
            .map_err(|source| internal_error("conversation metadata could not be read", source))?;
        let file_length = metadata.len();
        let modified = metadata.modified().map_err(|source| {
            internal_error("conversation modification time could not be read", source)
        })?;
        if let Some(position) = self.entries.iter().position(|entry| {
            entry.path == path && entry.file_length == file_length && entry.modified == modified
        }) {
            let entry = self
                .entries
                .remove(position)
                .expect("cache position exists");
            let offsets = entry.offsets.clone();
            self.entries.push_back(entry);
            return Ok(offsets);
        }
        self.entries.retain(|entry| entry.path != path);
        let offsets = build_offsets(path, None)?;
        if self.entries.len() == INDEX_CACHE_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(CachedConversationIndex {
            path: path.to_path_buf(),
            file_length,
            modified,
            offsets: offsets.clone(),
        });
        Ok(offsets)
    }
}

fn build_offsets(
    path: &Path,
    mut collected: Option<&mut dyn FnMut(ConversationMessage) -> StorageResult<()>>,
) -> StorageResult<ConversationOffsets> {
    let file = File::open(path)
        .map_err(|source| internal_error("conversation file could not be opened", source))?;
    let mut reader = RecordReader::new(BufReader::new(file));
    let mut message = Vec::new();
    let mut display = Vec::new();
    let mut ids = HashMap::new();
    let mut referenced_ids = HashSet::new();
    let mut referenced_runs = HashSet::new();
    let mut history = history_index::HistoryOffsets::default();
    let mut validation = agent_types::ToolExchangeValidator::default();
    while let Some(batch) = reader.next()? {
        history.record(&batch.history);
        for origin in batch.history.message_origins {
            referenced_ids.insert(origin.message_id);
            if let Some(run) = origin.run_id {
                referenced_runs.insert(run);
            }
        }
        for (index, value) in batch.messages.iter().enumerate() {
            validation.push(value).map_err(|source| {
                invalid_data_with_source("conversation tool exchange is invalid", source)
            })?;
            if ids
                .insert(message_id(value).clone(), message.len())
                .is_some()
            {
                return Err(invalid_data("conversation contains a duplicate message id"));
            }
            if value.is_transcript_visible()
                || matches!(value, ConversationMessage::ContextSummary(_))
            {
                display.push(message.len());
            }
            message.push(MessagePosition {
                offset: batch.offset,
                index,
            });
        }
        if let Some(collected) = &mut collected {
            for value in batch.messages {
                collected(value)?;
            }
        }
    }
    validation.finish().map_err(|source| {
        invalid_data_with_source("conversation tool exchange is invalid", source)
    })?;
    if referenced_ids.iter().any(|id| !ids.contains_key(id)) {
        return Err(invalid_data(
            "conversation history references a missing message",
        ));
    }
    if referenced_runs.iter().any(|run| !history.contains_run(run)) {
        return Err(invalid_data(
            "conversation message origin references a missing historical Run",
        ));
    }
    history.finish();
    Ok(ConversationOffsets {
        message: message.into(),
        display: display.into(),
        encoding: reader.encoding(),
        header: reader.header,
        history: Arc::new(history),
        locations: Arc::new(ids),
        validation: Arc::new(validation),
    })
}

/// offset 来自当前文件长度核验后的索引；一次只解码一帧，不把分页退化为全历史读取。
fn read_positions(
    path: &Path,
    positions: &[MessagePosition],
    encoding: Encoding,
) -> StorageResult<ConversationSnapshot> {
    let file = File::open(path)
        .map_err(|source| internal_error("conversation file could not be opened", source))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut offset = None;
    let mut frame_messages = Vec::new();
    let mut messages = Vec::with_capacity(positions.len());
    for position in positions {
        if offset != Some(position.offset) {
            reader
                .seek(SeekFrom::Start(position.offset))
                .map_err(|source| {
                    internal_error("conversation file could not be positioned", source)
                })?;
            line.clear();
            reader.read_until(b'\n', &mut line).map_err(|source| {
                internal_error("conversation record could not be read", source)
            })?;
            frame_messages = codec::decode_indexed_record(&line, encoding)?;
            offset = Some(position.offset);
        }
        let message = frame_messages
            .get(position.index)
            .ok_or_else(|| invalid_data("conversation index exceeds its commit frame"))?;
        messages.push(message.clone());
    }
    validate_snapshot(messages)
}

pub(super) fn encode_messages(messages: &[ConversationMessage]) -> StorageResult<Vec<u8>> {
    let mut bytes = Vec::new();
    for message in messages {
        serde_json::to_writer(&mut bytes, message)
            .map_err(|source| internal_error("conversation could not be encoded", source))?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

/// 物理路径由 Store 核实后，再用格式头绑定完整身份；只返回可靠正文，不装配执行。
pub(super) fn read_owned(
    path: &Path,
    owner: &assistant_protocol::ConversationOwner,
    generation: u64,
) -> StorageResult<ConversationSnapshot> {
    let document = read_document(path, owner, generation)?;
    if document.history.is_none() {
        return Err(invalid_data("conversation history header is missing"));
    }
    Ok(document.conversation)
}

pub(super) fn read_document(
    path: &Path,
    owner: &assistant_protocol::ConversationOwner,
    generation: u64,
) -> StorageResult<ConversationDocument> {
    let file = File::open(path)
        .map_err(|source| internal_error("conversation file could not be opened", source))?;
    let (conversation, facts, header) = decode_history(BufReader::new(file))?;
    if header
        .as_ref()
        .is_some_and(|header| &header.owner != owner || header.generation != generation)
    {
        return Err(invalid_data(
            "conversation history owner or generation does not match",
        ));
    }
    Ok(ConversationDocument {
        conversation,
        history: header.map(|_| facts),
    })
}

/// 新文件尚未发布为当前 generation；流式写入、同步并回读完整事实后才交给切换事务。
pub(super) fn write_generation(
    path: &Path,
    owner: &assistant_protocol::ConversationOwner,
    generation: u64,
    conversation: &ConversationSnapshot,
    history: &StoredHistoryFacts,
) -> StorageResult<()> {
    conversation
        .validate_tool_exchange_pairs()
        .map_err(|source| {
            invalid_data_with_source("replacement history splits a tool exchange", source)
        })?;
    let mut file = super::create_new_private_file(path)?;
    codec::write_generation(
        &mut file,
        owner,
        generation,
        conversation.messages.iter().cloned().map(Ok),
        history,
    )?;
    file.flush()
        .map_err(|source| internal_error("replacement history could not be flushed", source))?;
    file.sync_all().map_err(|source| {
        internal_error("replacement history could not be synchronized", source)
    })?;
    stream::verify_generation(
        path,
        owner,
        generation,
        conversation.messages.iter().cloned().map(Ok),
        history,
    )?;
    Ok(())
}

pub(super) fn create_empty(
    path: &Path,
    owner: &assistant_protocol::ConversationOwner,
    generation: u64,
) -> StorageResult<()> {
    write_generation(
        path,
        owner,
        generation,
        &ConversationSnapshot::new(Vec::new()),
        &StoredHistoryFacts::default(),
    )
}

pub(super) fn encode_history_append(
    operation_id: &str,
    messages: &[ConversationMessage],
    history: StoredHistoryFacts,
) -> StorageResult<Vec<u8>> {
    codec::encode_append(operation_id, messages, history)
}

/// staged payload 没有格式头；格式必须来自已核对的目标文件，不能依赖 payload 猜测。
pub(super) fn decode_append(
    payload: &[u8],
    framed: bool,
    owner: &assistant_protocol::ConversationOwner,
) -> StorageResult<ConversationDocument> {
    if framed {
        let frame = codec::decode_commit(payload)?;
        codec::validate_frame_owner(&frame, owner)?;
        Ok(ConversationDocument {
            conversation: validate_snapshot(frame.messages)?,
            history: Some(frame.history),
        })
    } else {
        Ok(ConversationDocument {
            conversation: decode(BufReader::new(payload))?,
            history: None,
        })
    }
}

/// 恢复回执时只读取第一条格式记录，不重复加载已经核实的完整正文。
pub(super) fn has_history_header(
    path: &Path,
    owner: &assistant_protocol::ConversationOwner,
    generation: u64,
) -> StorageResult<bool> {
    let file = File::open(path)
        .map_err(|source| internal_error("conversation file could not be opened", source))?;
    let mut line = Vec::new();
    BufReader::new(file)
        .read_until(b'\n', &mut line)
        .map_err(|source| internal_error("conversation header could not be read", source))?;
    let mut reader = RecordReader::new(std::io::Cursor::new(line));
    reader.next()?;
    if let Some(header) = reader.header {
        if &header.owner != owner || header.generation != generation {
            return Err(invalid_data(
                "conversation history owner or generation does not match",
            ));
        }
        Ok(true)
    } else {
        Ok(false)
    }
}

pub(super) fn decode(reader: impl BufRead) -> StorageResult<ConversationSnapshot> {
    decode_history(reader).map(|(conversation, _, _)| conversation)
}

/// 解码同一文件的正文和可靠事实；只读，不创建执行控制或任何磁盘历史索引。
fn decode_history(
    reader: impl BufRead,
) -> StorageResult<(
    ConversationSnapshot,
    StoredHistoryFacts,
    Option<HistoryHeader>,
)> {
    let mut reader = RecordReader::new(reader);
    let mut messages = Vec::new();
    let mut facts = StoredHistoryFacts::default();
    while let Some(batch) = reader.next()? {
        messages.extend(batch.messages);
        facts.extend(batch.history);
    }
    facts.normalize();
    let conversation = validate_snapshot(messages)?;
    let ids: HashSet<_> = conversation.messages.iter().map(message_id).collect();
    if facts
        .message_origins
        .iter()
        .any(|origin| !ids.contains(&origin.message_id))
    {
        return Err(invalid_data(
            "conversation history references a missing message",
        ));
    }
    let runs: HashSet<_> = facts.runs.iter().map(|run| &run.run_id).collect();
    if facts
        .message_origins
        .iter()
        .filter_map(|origin| origin.run_id.as_ref())
        .any(|run| !runs.contains(run))
    {
        return Err(invalid_data(
            "conversation message origin references a missing historical Run",
        ));
    }
    Ok((conversation, facts, reader.header))
}

fn validate_snapshot(messages: Vec<ConversationMessage>) -> StorageResult<ConversationSnapshot> {
    let mut ids = HashSet::new();
    if messages
        .iter()
        .any(|message| !ids.insert(message_id(message)))
    {
        return Err(invalid_data("conversation contains a duplicate message id"));
    }
    let snapshot = ConversationSnapshot::new(messages);
    snapshot.validate_tool_exchange_pairs().map_err(|source| {
        invalid_data_with_source("conversation tool exchange is invalid", source)
    })?;
    Ok(snapshot)
}

pub(super) fn reconcile_append(
    path: &Path,
    base_byte_length: u64,
    payload: &[u8],
) -> StorageResult<()> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| internal_error("conversation file could not be opened", source))?;
    let current_length = file
        .metadata()
        .map_err(|source| internal_error("conversation metadata could not be read", source))?
        .len();
    if current_length < base_byte_length {
        return Err(invalid_data(
            "conversation is shorter than the staged append base",
        ));
    }

    file.seek(SeekFrom::Start(base_byte_length))
        .map_err(|source| internal_error("conversation file could not be positioned", source))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)
        .map_err(|source| internal_error("conversation append tail could not be read", source))?;

    if tail == payload {
        // 文件内容完整不代表此前写入已经越过持久化边界；在提交 SQLite 元数据前再次同步。
        return file.sync_data().map_err(|source| {
            internal_error("conversation append could not be synchronized", source)
        });
    }
    if !payload.starts_with(&tail) {
        return Err(invalid_data(
            "conversation append tail conflicts with staged content",
        ));
    }

    // 空尾段和部分尾段都从已记录 base 重新写完整 payload，避免在半个 UTF-8/JSON token 后续写。
    file.set_len(base_byte_length).map_err(|source| {
        internal_error(
            "conversation partial append could not be rolled back",
            source,
        )
    })?;
    file.seek(SeekFrom::Start(base_byte_length))
        .map_err(|source| internal_error("conversation file could not be positioned", source))?;
    file.write_all(payload)
        .map_err(|source| internal_error("conversation append could not be written", source))?;
    file.flush()
        .map_err(|source| internal_error("conversation append could not be flushed", source))?;
    file.sync_data().map_err(|source| {
        internal_error("conversation append could not be synchronized", source)
    })?;
    Ok(())
}

pub(super) fn message_id(message: &ConversationMessage) -> &MessageId {
    match message {
        ConversationMessage::System(message) => &message.id,
        ConversationMessage::ContextSummary(message) => &message.id,
        ConversationMessage::User(message) => &message.id,
        ConversationMessage::Assistant(message) => &message.id,
        ConversationMessage::Tool(message) => &message.id,
    }
}

fn invalid_data_with_source(
    message: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> assistant_runtime::StoreError {
    assistant_runtime::StoreError::with_source(
        assistant_runtime::StoreErrorKind::InvalidData,
        message,
        source,
    )
}
