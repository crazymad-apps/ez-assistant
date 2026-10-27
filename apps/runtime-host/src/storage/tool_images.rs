//! Session `tool-images/` 的启动恢复扫描与当前 generation 引用收集。

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use agent_types::{ConversationMessage, ConversationSnapshot, ToolImageReference, ToolResultPart};
use assistant_protocol::{ChildTaskId, SessionId};
use rusqlite::params;

use super::{
    StorageEngine, StorageResult, body_path, child_body_path, child_task_directory, conversation,
    internal_error, invalid_data, positive_u64, sync_directory,
};

impl StorageEngine {
    /// staged append 与 pending exchange 修复完成后执行一次保守 mark-and-sweep。
    pub(super) fn recover_tool_images(&mut self) -> StorageResult<BTreeSet<String>> {
        let sessions = {
            let mut statement = self
                .connection
                .prepare("SELECT session_id, body_generation FROM sessions ORDER BY session_id")
                .map_err(|source| {
                    internal_error("tool image sessions could not be queried", source)
                })?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|source| {
                    internal_error("tool image sessions could not be read", source)
                })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|source| {
                internal_error("tool image session row could not be read", source)
            })?
        };
        let mut diagnostics = BTreeSet::new();
        for (raw_session_id, raw_generation) in sessions {
            let session_id = SessionId::new(raw_session_id.clone())
                .map_err(|_| invalid_data("stored session id is invalid"))?;
            let generation = positive_u64(raw_generation, "stored body generation is invalid")?;
            if self.recover_session_tool_images(&session_id, generation)? {
                diagnostics.insert(raw_session_id);
            }
        }
        Ok(diagnostics)
    }

    pub(super) fn recover_session_tool_images(
        &mut self,
        session_id: &SessionId,
        generation: u64,
    ) -> StorageResult<bool> {
        let session_directory = self.session_directory(session_id)?;
        let tool_image_directory = session_directory.join("tool-images");
        let mut references = BTreeSet::new();
        let mut all_conversations_readable =
            !self.unavailable_sessions.contains(session_id.as_str());
        let mut diagnostic = false;
        let mut historical_children = Vec::new();

        if all_conversations_readable {
            let path = body_path(&session_directory, generation);
            let owner = assistant_protocol::ConversationOwner::MainSession {
                session_id: session_id.clone(),
            };
            // 恢复本来就需要检查所有正文的图片引用；顺便建立位置索引，避免随后
            // Session 装配为了读取历史事实再次扫描整个文件。不缓存正文或图片内容。
            let document = if self.allow_legacy_history {
                conversation::read_document(&path, &owner, generation)
            } else {
                self.conversation_indexes
                    .read_owned(&path, &owner, generation)
                    .and_then(|conversation| {
                        self.conversation_indexes
                            .read_history(&path, &owner, generation)
                            .map(|history| conversation::ConversationDocument {
                                conversation,
                                history: Some(history.facts),
                            })
                    })
            };
            match document {
                Ok(conversation::ConversationDocument {
                    conversation,
                    history: Some(history),
                }) => {
                    collect_tool_images(&conversation, &mut references);
                    historical_children = history.child_tasks;
                }
                Ok(_) | Err(_) => {
                    all_conversations_readable = false;
                    diagnostic = true;
                }
            }
        }

        if !self.allow_legacy_history {
            match self.pending_child_catalog(session_id) {
                Ok(children) => historical_children.extend(children),
                Err(_) => {
                    all_conversations_readable = false;
                    diagnostic = true;
                }
            }
        }
        let children = self.session_child_generations(session_id, historical_children)?;
        for (child_task_id, generation) in children {
            if self.unavailable_child_tasks.contains(&(
                session_id.as_str().to_owned(),
                child_task_id.as_str().to_owned(),
            )) {
                all_conversations_readable = false;
                continue;
            }
            let directory = child_task_directory(&session_directory, &child_task_id);
            match conversation::read_owned(
                &child_body_path(&directory, generation),
                &assistant_protocol::ConversationOwner::ChildTask {
                    session_id: session_id.clone(),
                    child_task_id: child_task_id.clone(),
                },
                generation,
            ) {
                Ok(conversation) => collect_tool_images(&conversation, &mut references),
                Err(_) => {
                    all_conversations_readable = false;
                    diagnostic = true;
                }
            }
        }

        let mut changed = false;
        for entry in fs::read_dir(&tool_image_directory)
            .map_err(|source| internal_error("tool image directory could not be read", source))?
        {
            let entry = entry
                .map_err(|source| internal_error("tool image entry could not be read", source))?;
            let file_name = match entry.file_name().into_string() {
                Ok(file_name) => file_name,
                Err(_) => {
                    diagnostic = true;
                    continue;
                }
            };
            let metadata = fs::symlink_metadata(entry.path()).map_err(|source| {
                internal_error("tool image entry metadata could not be read", source)
            })?;
            if file_name.ends_with(".part") && metadata.file_type().is_file() {
                fs::remove_file(entry.path()).map_err(|source| {
                    internal_error("tool image temporary file could not be removed", source)
                })?;
                changed = true;
                continue;
            }
            let Some(reference) = reference_from_file_name(&file_name) else {
                diagnostic = true;
                continue;
            };
            if !metadata.file_type().is_file()
                || crate::image::validate_tool_image_file(&tool_image_directory, &reference)
                    .is_err()
            {
                diagnostic = true;
                continue;
            }
            if all_conversations_readable && !references.contains(&reference) {
                fs::remove_file(entry.path()).map_err(|source| {
                    internal_error("orphaned tool image could not be removed", source)
                })?;
                changed = true;
            }
        }

        for reference in references {
            if crate::image::validate_tool_image_file(&tool_image_directory, &reference).is_err() {
                diagnostic = true;
            }
        }
        if changed {
            sync_directory(&tool_image_directory)?;
        }
        Ok(diagnostic)
    }

    /// 主文件中的目录保存已结束 child 的阅读位置；尚未闭合父交换的真实控制补充在途位置。
    /// 此处只收集资源引用，不装配任务。主文件或任一 child 不可读时，调用方停止完整图片清理。
    fn session_child_generations(
        &self,
        session_id: &SessionId,
        historical_children: Vec<assistant_runtime::StoredChildTaskHistory>,
    ) -> StorageResult<BTreeMap<ChildTaskId, u64>> {
        let mut generations: BTreeMap<_, _> = historical_children
            .into_iter()
            .map(|task| (task.child_task_id, task.body_generation))
            .collect();
        let mut statement = self
            .connection
            .prepare(
                "SELECT child_task_id, body_generation
                 FROM child_tasks WHERE session_id = ?1
                 AND (?2 OR status IN ('accepted','running')) ORDER BY child_task_id",
            )
            .map_err(|source| {
                internal_error("tool image child tasks could not be queried", source)
            })?;
        let rows = statement
            .query_map(
                params![session_id.as_str(), self.allow_legacy_history],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(|source| internal_error("tool image child tasks could not be read", source))?;
        let controls = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| internal_error("tool image child row could not be read", source))?
            .into_iter()
            .map(|(child_task_id, generation)| {
                Ok((
                    ChildTaskId::new(child_task_id)
                        .map_err(|_| invalid_data("stored child task id is invalid"))?,
                    positive_u64(generation, "stored child body generation is invalid")?,
                ))
            })
            .collect::<StorageResult<Vec<_>>>()?;
        // 父工具交换尚未提交时，控制指针可能比父文件快照更新；不使用旧 generation 覆盖它。
        generations.extend(controls);
        Ok(generations)
    }
}

pub(super) fn collect_tool_images(
    conversation: &ConversationSnapshot,
    output: &mut BTreeSet<ToolImageReference>,
) {
    for message in &conversation.messages {
        let ConversationMessage::Tool(message) = message else {
            continue;
        };
        for part in message.result.content.as_parts() {
            if let ToolResultPart::Image { image } = part {
                output.insert(image.clone());
            }
        }
    }
}

pub(super) fn reference_from_file_name(file_name: &str) -> Option<ToolImageReference> {
    let media_type = match Path::new(file_name).extension()?.to_str()? {
        "jpg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => return None,
    };
    ToolImageReference::new(file_name.to_owned(), media_type).ok()
}
