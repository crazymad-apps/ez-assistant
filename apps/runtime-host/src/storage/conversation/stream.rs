//! generation 准备与回读逐条消费正文；正文只保留当前物理帧及工具交换闭包。
//! 定位键和只读元数据按 owner 归并，不把消息或工具输出拼成完整快照。

use std::{
    collections::{HashSet, VecDeque},
    fs::File,
    io::{BufReader, Write},
    path::Path,
};

use agent_types::{ConversationMessage, ToolExchangeValidator};
use assistant_protocol::ConversationOwner;
use assistant_runtime::StoredHistoryFacts;

use super::{
    StorageResult,
    codec::{self, RecordReader},
    internal_error, invalid_data, invalid_data_with_source,
};
use crate::storage::history::HistoryMessageKeys;

/// 仅提取迁出／裁尾需要的键及文件原有事实；不把正文拼成 ConversationSnapshot。
pub(in crate::storage) fn scan_document(
    path: &Path,
    owner: &ConversationOwner,
    generation: u64,
) -> StorageResult<(HistoryMessageKeys, Option<StoredHistoryFacts>)> {
    scan_append(path, owner, generation, &[])
}

/// 先核验可靠源，再继续使用同一调用 ID 作用域校验候选消息；不复制或重编码整份历史。
/// 返回值中的文件事实仍只来自可靠源，候选事实必须由 staged commit 单独冻结。
pub(in crate::storage) fn scan_append(
    path: &Path,
    owner: &ConversationOwner,
    generation: u64,
    appended: &[ConversationMessage],
) -> StorageResult<(HistoryMessageKeys, Option<StoredHistoryFacts>)> {
    let mut reader =
        RecordReader::new(BufReader::new(File::open(path).map_err(|source| {
            internal_error("conversation file could not be opened", source)
        })?));
    let mut keys = HistoryMessageKeys::default();
    let mut facts = StoredHistoryFacts::default();
    let mut validation = ToolExchangeValidator::default();
    while let Some(batch) = reader.next()? {
        for message in batch.messages {
            validation.push(&message).map_err(|source| {
                invalid_data_with_source("conversation tool exchange is invalid", source)
            })?;
            keys.record(&message)?;
        }
        facts.extend(batch.history);
    }
    validation.finish().map_err(|source| {
        invalid_data_with_source("conversation tool exchange is invalid", source)
    })?;
    if reader
        .header
        .as_ref()
        .is_some_and(|header| &header.owner != owner || header.generation != generation)
    {
        return Err(invalid_data(
            "conversation history owner or generation does not match",
        ));
    }
    facts.normalize();
    if facts
        .message_origins
        .iter()
        .any(|origin| !keys.messages.contains_key(&origin.message_id))
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
    for message in appended {
        validation.push(message).map_err(|source| {
            invalid_data_with_source("conversation append tool exchange is invalid", source)
        })?;
        keys.record(message)?;
    }
    validation.finish().map_err(|source| {
        invalid_data_with_source("conversation append splits a tool exchange", source)
    })?;
    Ok((keys, reader.header.map(|_| facts)))
}

/// 只在已核对的同 owner 源上运行；调用方持实例锁，最终再次核对源文件证据后才能发布。
/// 新文件独立创建，源文件不改写；任一步失败只留下未引用准备件。
pub(in crate::storage) fn write_upgraded_generation(
    source: &Path,
    target: &Path,
    owner: &ConversationOwner,
    source_generation: u64,
    target_generation: u64,
    message_count: u64,
    history: &StoredHistoryFacts,
) -> StorageResult<()> {
    // 源身份已在准备扫描中核对；这里仍检查头，防止调用方把另一 owner 作为消息来源。
    super::has_history_header(source, owner, source_generation)?;
    let messages = MessageReader::open(source)?;
    let mut file = crate::storage::create_new_private_file(target)?;
    codec::write_generation(&mut file, owner, target_generation, messages, history)?;
    file.flush()
        .map_err(|source| internal_error("replacement history could not be flushed", source))?;
    file.sync_all().map_err(|source| {
        internal_error("replacement history could not be synchronized", source)
    })?;
    let actual_count = verify_generation(
        target,
        owner,
        target_generation,
        MessageReader::open(source)?,
        history,
    )?;
    if u64::try_from(actual_count).ok() != Some(message_count) {
        return Err(invalid_data(
            "replacement history message count does not match",
        ));
    }
    Ok(())
}

/// 完整回读元数据并逐条比较正文，不能用消息数或首尾样本替代内容一致性核验。
pub(in crate::storage) fn verify_generation(
    path: &Path,
    owner: &ConversationOwner,
    generation: u64,
    messages: impl IntoIterator<Item = StorageResult<ConversationMessage>>,
    history: &StoredHistoryFacts,
) -> StorageResult<usize> {
    let (keys, actual) = scan_document(path, owner, generation)?;
    let mut expected = history.clone();
    expected.normalize();
    if actual.as_ref() != Some(&expected) {
        return Err(invalid_data("replacement history verification failed"));
    }
    let mut actual_messages = MessageReader::open(path)?;
    for expected in messages {
        if actual_messages.next().transpose()? != Some(expected?) {
            return Err(invalid_data(
                "replacement history message verification failed",
            ));
        }
    }
    if actual_messages.next().transpose()?.is_some() {
        return Err(invalid_data(
            "replacement history contains unexpected messages",
        ));
    }
    Ok(keys.messages.len())
}

/// 每次只保留一个物理帧；元数据帧不占正文序号，也不参与工具执行。
struct MessageReader {
    reader: RecordReader<BufReader<File>>,
    messages: VecDeque<ConversationMessage>,
}

impl MessageReader {
    fn open(path: &Path) -> StorageResult<Self> {
        let file = File::open(path)
            .map_err(|source| internal_error("conversation file could not be opened", source))?;
        Ok(Self {
            reader: RecordReader::new(BufReader::new(file)),
            messages: VecDeque::new(),
        })
    }
}

impl Iterator for MessageReader {
    type Item = StorageResult<ConversationMessage>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(message) = self.messages.pop_front() {
                return Some(Ok(message));
            }
            match self.reader.next() {
                Ok(Some(batch)) => self.messages = batch.messages.into(),
                Ok(None) => return None,
                Err(error) => return Some(Err(error)),
            }
        }
    }
}
