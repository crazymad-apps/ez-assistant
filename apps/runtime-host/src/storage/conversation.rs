//! 规范 Conversation 的 JSONL 编解码和文件提交原语。

use std::{
    collections::{HashSet, VecDeque},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use agent_types::{ConversationMessage, ConversationSnapshot, MessageId};

use super::{PRIVATE_FILE_MODE, StorageResult, internal_error, invalid_data};

const INDEX_CACHE_CAPACITY: usize = 32;

struct CachedConversationIndex {
    path: PathBuf,
    file_length: u64,
    display_offsets: Arc<[u64]>,
}

/// Store Worker 私有的可重建 LRU；只缓存字节 offset，不缓存 Conversation 正文。
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
        let file_length = path
            .metadata()
            .map_err(|source| internal_error("conversation metadata could not be read", source))?
            .len();
        let offsets = self.offsets(path, file_length)?;
        let total = offsets.len();
        let end = requested_end.unwrap_or(total).min(total);
        let start = end.saturating_sub(limit);
        let byte_start = offsets.get(start).copied().unwrap_or(file_length);
        let byte_end = offsets.get(end).copied().unwrap_or(file_length);
        let mut file = File::open(path)
            .map_err(|source| internal_error("conversation file could not be opened", source))?;
        file.seek(SeekFrom::Start(byte_start)).map_err(|source| {
            internal_error("conversation file could not be positioned", source)
        })?;
        let reader = BufReader::new(file.take(byte_end.saturating_sub(byte_start)));
        Ok(assistant_runtime::StoredConversationWindow {
            generation,
            start,
            end,
            total,
            conversation: decode(reader)?,
        })
    }

    fn offsets(&mut self, path: &Path, file_length: u64) -> StorageResult<Arc<[u64]>> {
        if let Some(position) = self
            .entries
            .iter()
            .position(|entry| entry.path == path && entry.file_length == file_length)
        {
            let entry = self
                .entries
                .remove(position)
                .expect("cache position exists");
            let offsets = entry.display_offsets.clone();
            self.entries.push_back(entry);
            return Ok(offsets);
        }
        self.entries.retain(|entry| entry.path != path);
        let display_offsets: Arc<[u64]> =
            Arc::from(build_display_offsets(path)?.into_boxed_slice());
        if self.entries.len() == INDEX_CACHE_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(CachedConversationIndex {
            path: path.to_path_buf(),
            file_length,
            display_offsets: display_offsets.clone(),
        });
        Ok(display_offsets)
    }
}

fn build_display_offsets(path: &Path) -> StorageResult<Vec<u64>> {
    let file = File::open(path)
        .map_err(|source| internal_error("conversation file could not be opened", source))?;
    let mut reader = BufReader::new(file);
    let mut offsets = Vec::new();
    let mut line = Vec::new();
    let mut offset = 0_u64;
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|source| internal_error("conversation file could not be indexed", source))?;
        if read == 0 {
            break;
        }
        if line.last() != Some(&b'\n') {
            return Err(invalid_data(
                "conversation contains an incomplete JSONL record",
            ));
        }
        let message: ConversationMessage = serde_json::from_slice(&line[..line.len() - 1])
            .map_err(|source| invalid_data_with_source("conversation JSONL is invalid", source))?;
        if matches!(
            message,
            ConversationMessage::User(_) | ConversationMessage::Assistant(_)
        ) {
            offsets.push(offset);
        }
        offset = offset
            .checked_add(u64::try_from(read).map_err(|source| {
                internal_error("conversation index offset exceeds storage range", source)
            })?)
            .ok_or_else(|| invalid_data("conversation index offset exceeds storage range"))?;
    }
    Ok(offsets)
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

pub(super) fn read(path: &Path) -> StorageResult<ConversationSnapshot> {
    let file = File::open(path)
        .map_err(|source| internal_error("conversation file could not be opened", source))?;
    decode(BufReader::new(file))
}

pub(super) fn decode(mut reader: impl BufRead) -> StorageResult<ConversationSnapshot> {
    let mut messages = Vec::new();
    let mut message_ids = HashSet::new();
    let mut line = Vec::new();

    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|source| internal_error("conversation file could not be read", source))?;
        if read == 0 {
            break;
        }
        if line.last() != Some(&b'\n') {
            return Err(invalid_data(
                "conversation contains an incomplete JSONL record",
            ));
        }
        line.pop();
        if line.is_empty() {
            return Err(invalid_data("conversation contains an empty JSONL record"));
        }
        let message: ConversationMessage = serde_json::from_slice(&line)
            .map_err(|source| invalid_data_with_source("conversation JSONL is invalid", source))?;
        if !message_ids.insert(message_id(&message).clone()) {
            return Err(invalid_data("conversation contains a duplicate message id"));
        }
        messages.push(message);
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

pub(super) fn write_replacement(path: &Path, payload: &[u8]) -> StorageResult<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .map_err(|source| {
            internal_error("replacement conversation could not be created", source)
        })?;
    file.write_all(payload).map_err(|source| {
        internal_error("replacement conversation could not be written", source)
    })?;
    file.flush().map_err(|source| {
        internal_error("replacement conversation could not be flushed", source)
    })?;
    file.sync_data().map_err(|source| {
        internal_error("replacement conversation could not be synchronized", source)
    })?;
    Ok(())
}

pub(super) fn validate_candidate(
    current: &ConversationSnapshot,
    appended: &[ConversationMessage],
) -> StorageResult<()> {
    let mut candidate = current.messages.clone();
    candidate.extend_from_slice(appended);
    let encoded = encode_messages(&candidate)?;
    decode(BufReader::new(encoded.as_slice())).map(|_| ())
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
