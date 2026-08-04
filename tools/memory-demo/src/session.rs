//! Demo 私有 Session 文件及冻结 Prompt 生命周期。

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use agent_memory::{
    PinnedMemoryLimits, PinnedMemorySnapshot, PinnedMemorySnapshotInput, PinnedMemoryStore,
};
use agent_model::SystemPromptSnapshot;
use agent_types::{AssistantMessage, ConversationSnapshot};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::atomic_json::{self, AtomicJsonError, AtomicJsonWriter};

const SESSION_VERSION: u32 = 1;

/// 创建新 Session 时使用结构化输入；恢复 Session 不再使用这些输入重建 Prompt。
#[derive(Clone, Debug)]
pub(crate) struct NewSessionInput {
    pub(crate) id: String,
    pub(crate) instruction_parts: Vec<String>,
    pub(crate) recall_part: String,
    pub(crate) pinned_description: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PendingToolExchange {
    pub(crate) receipt: String,
    pub(crate) assistant: AssistantMessage,
}

/// Session 文件只把 completed conversation 投影给模型；pending 单独保存用于恢复。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct SessionRecord {
    pub(crate) version: u32,
    pub(crate) id: String,
    pub(crate) system_prompt: SystemPromptSnapshot,
    pub(crate) conversation: ConversationSnapshot,
    pub(crate) next_exchange_id: u64,
    pub(crate) pending_exchange: Option<PendingToolExchange>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SessionContinuation {
    pub(crate) system_prompt: SystemPromptSnapshot,
    pub(crate) conversation: ConversationSnapshot,
}

#[derive(Debug, Error)]
pub(crate) enum SessionError {
    #[error("invalid demo session: {0}")]
    Invalid(String),
    #[error("demo session was not found")]
    NotFound,
    #[error("demo session I/O failed: {0}")]
    Io(String),
}

/// 创建新 Session：读取最新 Store、渲染 Pinned Part，然后一次性冻结并保存最终 Prompt。
pub(crate) async fn create_new_session(
    sessions_dir: &Path,
    input: NewSessionInput,
    store: Arc<dyn PinnedMemoryStore>,
    limits: &PinnedMemoryLimits,
) -> Result<SessionRecord, SessionError> {
    validate_session_id(&input.id)?;
    tokio::fs::create_dir_all(sessions_dir)
        .await
        .map_err(|error| SessionError::Io(error.to_string()))?;
    let path = session_path(sessions_dir, &input.id);
    if tokio::fs::try_exists(&path)
        .await
        .map_err(|error| SessionError::Io(error.to_string()))?
    {
        return Err(SessionError::Invalid(
            "session id already exists".to_owned(),
        ));
    }

    let entries = store
        .list(CancellationToken::new())
        .await
        .map_err(|error| SessionError::Io(error.to_string()))?;
    let pinned = PinnedMemorySnapshot::render(
        PinnedMemorySnapshotInput {
            description: input.pinned_description,
            entries,
        },
        limits,
    )
    .map_err(|error| SessionError::Invalid(error.to_string()))?;
    let mut parts = input.instruction_parts;
    parts.push(input.recall_part);
    parts.push(pinned.into_content());
    let record = SessionRecord {
        version: SESSION_VERSION,
        id: input.id,
        system_prompt: SystemPromptSnapshot::new(parts),
        conversation: ConversationSnapshot::default(),
        next_exchange_id: 1,
        pending_exchange: None,
    };
    save_session(&path, &record, AtomicJsonWriter::default()).await?;
    Ok(record)
}

/// 恢复只读取并校验 Session 成品，不接触 Pinned Store 或创建参数。
pub(crate) async fn restore_session(
    sessions_dir: &Path,
    id: &str,
) -> Result<SessionRecord, SessionError> {
    validate_session_id(id)?;
    let path = session_path(sessions_dir, id);
    let record = atomic_json::read::<SessionRecord>(&path)
        .await
        .map_err(map_read_error)?
        .ok_or(SessionError::NotFound)?;
    validate_record(&record, id)?;
    Ok(record)
}

/// 分支复制原 Session 的 Prompt 与规范对话，不读取最新 Store 重建 Prompt。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn branch_session(
    sessions_dir: &Path,
    source: &SessionRecord,
    new_id: String,
) -> Result<SessionRecord, SessionError> {
    validate_record(source, &source.id)?;
    if source.pending_exchange.is_some() {
        return Err(SessionError::Invalid(
            "cannot branch a session with a pending tool exchange".to_owned(),
        ));
    }
    validate_session_id(&new_id)?;
    tokio::fs::create_dir_all(sessions_dir)
        .await
        .map_err(|error| SessionError::Io(error.to_string()))?;
    let path = session_path(sessions_dir, &new_id);
    if tokio::fs::try_exists(&path)
        .await
        .map_err(|error| SessionError::Io(error.to_string()))?
    {
        return Err(SessionError::Invalid(
            "session id already exists".to_owned(),
        ));
    }
    let branch = SessionRecord {
        version: SESSION_VERSION,
        id: new_id,
        system_prompt: source.system_prompt.clone(),
        conversation: source.conversation.clone(),
        next_exchange_id: 1,
        pending_exchange: None,
    };
    save_session(&path, &branch, AtomicJsonWriter::default()).await?;
    Ok(branch)
}

/// 同一 Session 的 continuation 只复制已经恢复出的冻结输入。
pub(crate) fn continuation_session(
    source: &SessionRecord,
) -> Result<SessionContinuation, SessionError> {
    validate_record(source, &source.id)?;
    if source.pending_exchange.is_some() {
        return Err(SessionError::Invalid(
            "cannot continue before a pending tool exchange is recovered".to_owned(),
        ));
    }
    Ok(SessionContinuation {
        system_prompt: source.system_prompt.clone(),
        conversation: source.conversation.clone(),
    })
}

pub(crate) fn session_path(sessions_dir: &Path, id: &str) -> PathBuf {
    sessions_dir.join(format!("{id}.json"))
}

pub(crate) async fn save_session(
    path: &Path,
    record: &SessionRecord,
    writer: AtomicJsonWriter,
) -> Result<(), SessionError> {
    writer
        .write(path, record)
        .await
        .map_err(|error| SessionError::Io(error.to_string()))
}

pub(crate) fn validate_record(
    record: &SessionRecord,
    expected_id: &str,
) -> Result<(), SessionError> {
    if record.version != SESSION_VERSION {
        return Err(SessionError::Invalid(
            "unsupported session file version".to_owned(),
        ));
    }
    validate_session_id(&record.id)?;
    if record.id != expected_id {
        return Err(SessionError::Invalid(
            "session id does not match the requested file".to_owned(),
        ));
    }
    if record.next_exchange_id == 0 {
        return Err(SessionError::Invalid(
            "next exchange id must be greater than zero".to_owned(),
        ));
    }
    record
        .conversation
        .validate_tool_exchange_pairs()
        .map_err(|error| SessionError::Invalid(error.to_string()))?;
    if let Some(pending) = &record.pending_exchange {
        validate_pending_exchange(record, pending)?;
    }
    Ok(())
}

fn validate_pending_exchange(
    record: &SessionRecord,
    pending: &PendingToolExchange,
) -> Result<(), SessionError> {
    let sequence = pending
        .receipt
        .strip_prefix("exchange_")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|sequence| {
            *sequence > 0 && sequence.checked_add(1) == Some(record.next_exchange_id)
        })
        .ok_or_else(|| SessionError::Invalid("pending exchange receipt is invalid".to_owned()))?;
    if pending.receipt != format!("exchange_{sequence:010}") {
        return Err(SessionError::Invalid(
            "pending exchange receipt is not canonical".to_owned(),
        ));
    }

    let mut pending_ids = HashSet::new();
    let mut call_count = 0;
    for part in &pending.assistant.parts {
        if let agent_types::AssistantPart::ToolCall(call) = part {
            call_count += 1;
            if !pending_ids.insert(call.id.clone()) {
                return Err(SessionError::Invalid(
                    "pending exchange contains duplicate tool call ids".to_owned(),
                ));
            }
        }
    }
    if call_count == 0 {
        return Err(SessionError::Invalid(
            "pending exchange does not contain a tool call".to_owned(),
        ));
    }
    for message in &record.conversation.messages {
        if let agent_types::ConversationMessage::Assistant(assistant) = message {
            for part in &assistant.parts {
                if let agent_types::AssistantPart::ToolCall(call) = part
                    && pending_ids.contains(&call.id)
                {
                    return Err(SessionError::Invalid(
                        "pending exchange reuses a completed tool call id".to_owned(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_session_id(id: &str) -> Result<(), SessionError> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SessionError::Invalid(
            "session id must contain only ASCII letters, digits, '-' or '_'".to_owned(),
        ));
    }
    Ok(())
}

fn map_read_error(error: AtomicJsonError) -> SessionError {
    match error {
        AtomicJsonError::InvalidData(_) => {
            SessionError::Invalid("session JSON is invalid".to_owned())
        }
        AtomicJsonError::Io(message) => SessionError::Io(message),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, num::NonZeroUsize};

    use agent_memory::{
        MemoryPropertyValue, PinnedMemoryCategory, PinnedMemoryDraft, PinnedMemoryStore,
    };

    use crate::pinned_store::DemoPinnedMemoryStore;

    use super::*;

    fn limits() -> PinnedMemoryLimits {
        PinnedMemoryLimits {
            max_entries: NonZeroUsize::new(8).expect("non-zero"),
            max_id_bytes: NonZeroUsize::new(32).expect("non-zero"),
            max_category_bytes: NonZeroUsize::new(32).expect("non-zero"),
            max_content_bytes: NonZeroUsize::new(256).expect("non-zero"),
            max_attributes_per_entry: NonZeroUsize::new(8).expect("non-zero"),
            max_attribute_key_bytes: NonZeroUsize::new(32).expect("non-zero"),
            max_attribute_string_bytes: NonZeroUsize::new(128).expect("non-zero"),
            max_description_bytes: NonZeroUsize::new(256).expect("non-zero"),
            max_snapshot_bytes: NonZeroUsize::new(8192).expect("non-zero"),
        }
    }

    fn input(id: &str) -> NewSessionInput {
        NewSessionInput {
            id: id.to_owned(),
            instruction_parts: vec!["You are the memory demo assistant.".to_owned()],
            recall_part: "Use recall_memory for historical reference data.".to_owned(),
            pinned_description: "These entries are frozen for this session.".to_owned(),
        }
    }

    fn draft(content: &str) -> PinnedMemoryDraft {
        PinnedMemoryDraft {
            category: PinnedMemoryCategory::new("preference").expect("valid category"),
            content: content.to_owned(),
            attributes: BTreeMap::from([(
                "scope".to_owned(),
                MemoryPropertyValue::String("demo".to_owned()),
            )]),
        }
    }

    #[tokio::test]
    async fn existing_session_remains_frozen_while_new_session_reads_latest_store() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store =
            DemoPinnedMemoryStore::open(directory.path().join("pinned-memory.json"), limits())
                .await
                .expect("open store");
        store
            .pin(draft("initial preference"), CancellationToken::new())
            .await
            .expect("pin initial entry");
        let sessions = directory.path().join("sessions");
        let first = create_new_session(
            &sessions,
            input("session_1"),
            Arc::clone(&store) as Arc<dyn PinnedMemoryStore>,
            &limits(),
        )
        .await
        .expect("create first session");
        store
            .pin(draft("later preference"), CancellationToken::new())
            .await
            .expect("pin later entry");
        let second = create_new_session(
            &sessions,
            input("session_2"),
            Arc::clone(&store) as Arc<dyn PinnedMemoryStore>,
            &limits(),
        )
        .await
        .expect("create second session");

        assert!(
            first
                .system_prompt
                .parts()
                .last()
                .expect("pinned part")
                .contains("initial preference")
        );
        assert!(
            !first
                .system_prompt
                .parts()
                .last()
                .expect("pinned part")
                .contains("later preference")
        );
        assert!(
            second
                .system_prompt
                .parts()
                .last()
                .expect("pinned part")
                .contains("later preference")
        );
        assert_eq!(
            restore_session(&sessions, "session_1")
                .await
                .expect("restore first session")
                .system_prompt,
            first.system_prompt
        );
    }

    #[tokio::test]
    async fn restore_after_object_rebuild_and_branch_never_read_latest_store() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("pinned-memory.json");
        let store = DemoPinnedMemoryStore::open(path.clone(), limits())
            .await
            .expect("open store");
        store
            .pin(draft("frozen"), CancellationToken::new())
            .await
            .expect("pin frozen entry");
        let sessions = directory.path().join("sessions");
        let created = create_new_session(
            &sessions,
            input("original"),
            Arc::clone(&store) as Arc<dyn PinnedMemoryStore>,
            &limits(),
        )
        .await
        .expect("create session");
        drop(store);
        let rebuilt = DemoPinnedMemoryStore::open(path, limits())
            .await
            .expect("rebuild store");
        rebuilt
            .pin(draft("new store value"), CancellationToken::new())
            .await
            .expect("modify rebuilt store");
        drop(rebuilt);

        let restored = restore_session(&sessions, "original")
            .await
            .expect("restore session");
        assert_eq!(restored.system_prompt, created.system_prompt);
        let branch = branch_session(&sessions, &restored, "branch".to_owned())
            .await
            .expect("branch session");
        assert_eq!(branch.system_prompt, created.system_prompt);
        assert_eq!(
            continuation_session(&restored)
                .expect("continuation")
                .system_prompt,
            created.system_prompt
        );
    }

    #[tokio::test]
    async fn missing_corrupt_incompatible_and_traversal_sessions_fail() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let sessions = directory.path().join("sessions");
        tokio::fs::create_dir_all(&sessions)
            .await
            .expect("create sessions directory");
        assert!(matches!(
            restore_session(&sessions, "missing").await,
            Err(SessionError::NotFound)
        ));
        std::fs::write(session_path(&sessions, "broken"), b"{").expect("write broken session");
        assert!(matches!(
            restore_session(&sessions, "broken").await,
            Err(SessionError::Invalid(_))
        ));
        assert!(matches!(
            restore_session(&sessions, "../escape").await,
            Err(SessionError::Invalid(_))
        ));

        let incompatible = SessionRecord {
            version: 2,
            id: "old".to_owned(),
            system_prompt: SystemPromptSnapshot::default(),
            conversation: ConversationSnapshot::default(),
            next_exchange_id: 1,
            pending_exchange: None,
        };
        AtomicJsonWriter::default()
            .write(&session_path(&sessions, "old"), &incompatible)
            .await
            .expect("write incompatible session");
        assert!(matches!(
            restore_session(&sessions, "old").await,
            Err(SessionError::Invalid(_))
        ));
    }
}
