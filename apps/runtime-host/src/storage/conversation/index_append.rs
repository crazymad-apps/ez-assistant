//! 复用可靠前缀的定位及调用 ID 校验状态；只在文件与 SQL 回执均提交后推进索引。

use assistant_protocol::ConversationOwner;

use super::*;

impl ConversationIndexCache {
    /// 调用方已拒绝 pending 回执并持有 Store Worker 的串行所有权。
    /// 只校验候选批次，不提前把未提交内容发布到缓存；元数据沿同一可靠索引读取。
    pub(in crate::storage) fn validate_append(
        &mut self,
        path: &Path,
        owner: &ConversationOwner,
        generation: u64,
        messages: &[ConversationMessage],
    ) -> StorageResult<Option<StoredHistoryFacts>> {
        let offsets = self.offsets(path)?;
        if offsets
            .header
            .as_ref()
            .is_some_and(|header| &header.owner != owner || header.generation != generation)
        {
            return Err(invalid_data(
                "conversation history owner or generation does not match",
            ));
        }
        validate_messages(&offsets, messages)?;
        Ok(offsets.header.as_ref().map(|_| offsets.history.snapshot()))
    }

    /// 写文件前再次核对前缀证据。恢复或外部改动后的旧索引不能参与增量推进。
    pub(in crate::storage) fn retain_current_prefix(&mut self, path: &Path) {
        let metadata = path.metadata().ok();
        self.entries.retain(|entry| {
            entry.path != path
                || metadata.as_ref().is_some_and(|metadata| {
                    entry.file_length == metadata.len()
                        && metadata.modified().ok() == Some(entry.modified)
                })
        });
    }

    /// 文件尾已同步、SQL 完成事务已提交且回执已删除。只更新可丢弃的派生索引；
    /// 没有匹配的可靠前缀、格式不匹配或构建失败时丢弃缓存，由下次读取完整重建。
    /// 不让缓存失败把已可靠提交的业务操作报告成失败，也不重放任何工具。
    pub(in crate::storage) fn record_committed_append(
        &mut self,
        path: &Path,
        base_length: u64,
        payload_length: usize,
        owner: &ConversationOwner,
        generation: u64,
        batch: &ConversationDocument,
    ) {
        let Some(position) = self.entries.iter().position(|entry| entry.path == path) else {
            return;
        };
        let mut entry = self
            .entries
            .remove(position)
            .expect("cache position exists");
        let Some(history) = batch.history.as_ref() else {
            return;
        };
        if entry.file_length != base_length
            || entry
                .offsets
                .header
                .as_ref()
                .is_none_or(|header| &header.owner != owner || header.generation != generation)
        {
            return;
        }
        let Ok(metadata) = path.metadata() else {
            return;
        };
        let Ok(modified) = metadata.modified() else {
            return;
        };
        if base_length.checked_add(payload_length as u64) != Some(metadata.len()) {
            return;
        }
        let Ok(offsets) = append_offsets(
            entry.offsets,
            base_length,
            &batch.conversation.messages,
            history,
        ) else {
            return;
        };
        entry.offsets = offsets;
        entry.file_length = metadata.len();
        entry.modified = modified;
        self.entries.push_back(entry);
    }
}

fn validate_messages(
    offsets: &ConversationOffsets,
    messages: &[ConversationMessage],
) -> StorageResult<agent_types::ToolExchangeValidator> {
    let mut validation = (*offsets.validation).clone();
    let mut ids = HashSet::new();
    for message in messages {
        let id = message_id(message);
        if offsets.locations.contains_key(id) || !ids.insert(id) {
            return Err(invalid_data("conversation contains a duplicate message id"));
        }
        validation.push(message).map_err(|source| {
            invalid_data_with_source("conversation append tool exchange is invalid", source)
        })?;
    }
    validation.finish().map_err(|source| {
        invalid_data_with_source("conversation append splits a tool exchange", source)
    })?;
    Ok(validation)
}

fn append_offsets(
    mut offsets: ConversationOffsets,
    offset: u64,
    messages: &[ConversationMessage],
    history: &StoredHistoryFacts,
) -> StorageResult<ConversationOffsets> {
    let validation = validate_messages(&offsets, messages)?;
    let mut positions = offsets.message.to_vec();
    let mut display = offsets.display.to_vec();
    let locations = Arc::make_mut(&mut offsets.locations);
    for (index, message) in messages.iter().enumerate() {
        locations.insert(message_id(message).clone(), positions.len());
        if message.is_transcript_visible()
            || matches!(message, ConversationMessage::ContextSummary(_))
        {
            display.push(positions.len());
        }
        positions.push(MessagePosition { offset, index });
    }
    let history_offsets = Arc::make_mut(&mut offsets.history);
    history_offsets.record(history);
    history_offsets.finish();
    if history.message_origins.iter().any(|origin| {
        !locations.contains_key(&origin.message_id)
            || origin
                .run_id
                .as_ref()
                .is_some_and(|run| !history_offsets.contains_run(run))
    }) {
        return Err(invalid_data(
            "conversation append references missing history",
        ));
    }
    offsets.message = positions.into();
    offsets.display = display.into();
    offsets.validation = Arc::new(validation);
    Ok(offsets)
}
