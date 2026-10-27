//! Fork 的纯历史裁切规则；真实资源复制和可靠提交仍归各 Store。

use std::collections::{HashMap, HashSet};

use agent_types::{
    AssistantPart, ConversationMessage, ConversationSnapshot, MessageId, ToolResultPart,
};
use assistant_protocol::{FeedbackStatus, SessionId};

use super::{StoredChildTaskHistory, StoredHistoryFacts, StoredMessageOrigin};
use crate::{ForkedSessionCommand, StoreError, StoreErrorKind};

impl StoredHistoryFacts {
    /// 调用方已在源 owner 的可靠 generation 内取得事实并验证规范前缀；这里只裁切只读归属。
    /// 控制消息的新 ID 通过既有 Fork 绑定核对，普通消息保留原 ID。未知旧归属保持未知；
    /// 不创建 Input/Run/child 控制、不取锁或执行 I/O。命令、选择和资源仍由原 Fork 流程改绑，
    /// child 描述仅选择可靠父结果；调用方须先准备正文／资源，缺失时明确标为不可用，
    /// 不能凭此函数补造可读文件。
    ///
    /// # Errors
    /// 控制消息来源缺失、重映射冲突或前缀消息身份重复时返回冲突。
    pub fn fork_lineage(
        &self,
        target_session: &SessionId,
        prefix: &ConversationSnapshot,
        commands: &[ForkedSessionCommand],
    ) -> Result<Self, StoreError> {
        let mut old_to_new = HashMap::new();
        let mut new_to_old = HashMap::new();
        let command_inputs = commands
            .iter()
            .map(|copied| {
                (
                    copied.source_input_id.clone(),
                    copied.command.input_id.clone(),
                )
            })
            .collect::<HashMap<_, _>>();
        for copied in commands {
            let source = self
                .commands
                .iter()
                .find(|source| source.input_id == copied.source_input_id)
                .ok_or_else(|| conflict("fork command source history is missing"))?;
            if old_to_new
                .insert(
                    source.user_message_id.clone(),
                    copied.command.user_message_id.clone(),
                )
                .is_some()
                || new_to_old
                    .insert(
                        copied.command.user_message_id.clone(),
                        source.user_message_id.clone(),
                    )
                    .is_some()
            {
                return Err(conflict("fork command message mapping is ambiguous"));
            }
        }
        let ordered = prefix
            .messages
            .iter()
            .map(message_id)
            .map(|id| (new_to_old.get(id).unwrap_or(id).clone(), id.clone()))
            .collect::<Vec<_>>();
        let retained = ordered.iter().map(|(old, _)| old).collect::<HashSet<_>>();
        if retained.len() != ordered.len() {
            return Err(conflict(
                "fork prefix contains duplicate message identities",
            ));
        }
        let origins = self
            .message_origins
            .iter()
            .map(|origin| (&origin.message_id, origin))
            .collect::<HashMap<_, _>>();
        // 单次按规范消息顺序建立最后保留点，避免每个 Run 都扫描完整前缀。
        let mut last_retained = HashMap::new();
        for (old, new) in &ordered {
            if let Some(run_id) = origins.get(old).and_then(|origin| origin.run_id.as_ref()) {
                last_retained.insert(run_id, new);
            }
        }
        let excluded_runs = self
            .message_origins
            .iter()
            .filter(|origin| !retained.contains(&origin.message_id))
            .filter_map(|origin| origin.run_id.as_ref())
            .collect::<HashSet<_>>();
        let mut selected_runs = self
            .message_origins
            .iter()
            .filter(|origin| retained.contains(&origin.message_id))
            .filter_map(|origin| origin.run_id.as_ref())
            .collect::<HashSet<_>>();
        let retained_outputs = prefix
            .messages
            .iter()
            .filter(|message| {
                matches!(
                    message,
                    ConversationMessage::Assistant(_) | ConversationMessage::Tool(_)
                )
            })
            .map(message_id)
            .map(|id| new_to_old.get(id).unwrap_or(id))
            .collect::<HashSet<_>>();
        for run in &self.runs {
            if run
                .last_message_id
                .as_ref()
                .is_some_and(|id| retained_outputs.contains(id))
            {
                selected_runs.insert(&run.run_id);
            }
        }
        let mut selected_attempts = HashMap::new();
        for run in self
            .runs
            .iter()
            .filter(|run| selected_runs.contains(&run.run_id))
        {
            if let Some(input_id) = &run.input_id {
                selected_attempts
                    .entry(input_id)
                    .and_modify(|attempt: &mut u32| *attempt = (*attempt).max(run.attempt))
                    .or_insert(run.attempt);
            }
        }
        let positions = ordered
            .iter()
            .enumerate()
            .map(|(index, (old, _))| (old, index))
            .collect::<HashMap<_, _>>();
        let retained_inputs = self
            .inputs
            .iter()
            .filter(|input| retained.contains(&input.user_message_id))
            .map(|input| &input.input_id)
            .collect::<HashSet<_>>();
        let mut history = Self::default();
        for source in &self.runs {
            // 共享旧 User 不证明 retry 的结束位置。只接纳可见归属、已选择 attempt
            // 之前的同 Input 尝试，或可靠结束锚点严格位于截点之前的无输出尝试。
            let earlier_attempt = source
                .input_id
                .as_ref()
                .and_then(|id| selected_attempts.get(id))
                .is_some_and(|attempt| source.attempt <= *attempt);
            let ended_before_cut = source.status.is_some_and(|status| status.is_terminal())
                && source
                    .input_id
                    .as_ref()
                    .is_some_and(|id| retained_inputs.contains(id))
                && source
                    .finished_after_message_id
                    .as_ref()
                    .and_then(|id| positions.get(id))
                    .is_some_and(|position| *position < ordered.len().saturating_sub(1));
            if !selected_runs.contains(&source.run_id) && !earlier_attempt && !ended_before_cut {
                continue;
            }
            let mut run = source.clone();
            run.partial |= excluded_runs.contains(&run.run_id)
                || !run.status.is_some_and(|status| status.is_terminal())
                || run
                    .last_message_id
                    .as_ref()
                    .is_some_and(|id| !retained.contains(id))
                || run
                    .finished_after_message_id
                    .as_ref()
                    .is_some_and(|id| !retained.contains(id));
            if run.partial {
                run.status = None;
                run.finished_at_ms = None;
                run.error = None;
                run.cancel_requested = false;
                run.finished_after_message_id = None;
            }
            run.finished_after_message_id = run
                .finished_after_message_id
                .filter(|id| retained.contains(id))
                .map(|id| old_to_new.get(&id).cloned().unwrap_or(id));
            run.last_message_id = last_retained
                .get(&run.run_id)
                .map(|id| (*id).clone())
                .or_else(|| {
                    source
                        .last_message_id
                        .as_ref()
                        .filter(|id| retained.contains(id))
                        .map(|id| old_to_new.get(id).unwrap_or(id).clone())
                });
            run.user_message_id = run
                .user_message_id
                .filter(|id| retained.contains(id))
                .map(|id| old_to_new.get(&id).cloned().unwrap_or(id));
            run.input_id = run.input_id.filter(|id| retained_inputs.contains(id));
            run.inherited = true;
            history.runs.push(run);
        }
        for source in &self.inputs {
            if retained.contains(&source.user_message_id) {
                let mut input = source.clone();
                input.user_message_id = old_to_new
                    .get(&input.user_message_id)
                    .cloned()
                    .unwrap_or(input.user_message_id);
                input.inherited = true;
                history.inputs.push(input);
            }
        }
        history.message_origins = ordered
            .iter()
            .map(|(old, new)| {
                let mut origin = origins
                    .get(old)
                    .map(|origin| (*origin).clone())
                    .unwrap_or_else(|| StoredMessageOrigin {
                        message_id: old.clone(),
                        run_id: None,
                        input_id: None,
                        attempt: None,
                        step: None,
                        inherited: true,
                    });
                origin.message_id = new.clone();
                origin.input_id = origin
                    .input_id
                    .map(|id| command_inputs.get(&id).cloned().unwrap_or(id));
                origin.inherited = true;
                origin
            })
            .collect();
        let retained_text_ids = retained
            .iter()
            .map(|id| id.as_str())
            .collect::<HashSet<_>>();
        history.feedback = self
            .feedback
            .iter()
            .filter(|feedback| retained_text_ids.contains(feedback.message_id.as_str()))
            .cloned()
            .collect();
        // 部分轮次可能在截点后才回答同一 call 的问题；不复制其未来处置快照。
        // 保留的规范 Ask/Tool 消息本身仍可阅读；完整轮次的终态问答按目标 Session 改绑。
        let complete_runs = history
            .runs
            .iter()
            .filter(|run| !run.partial)
            .map(|run| &run.run_id)
            .collect::<HashSet<_>>();
        history.questions = self
            .questions
            .iter()
            .filter(|question| {
                question.status != FeedbackStatus::Pending
                    && complete_runs.contains(&question.run_id)
            })
            .cloned()
            .map(|mut question| {
                question.session_id = target_session.clone();
                question
            })
            .collect();
        history.child_tasks = self.fork_child_tasks(prefix);
        history.normalize();
        Ok(history)
    }

    /// 只选择同一 Assistant 的 delegate_task 调用及其结构化结果证明的终态 child。
    /// 已有目录负责身份，规范消息负责截点；不扫描自由文本、不继承在途任务。
    /// 返回的描述改绑到新 owner 的第一代，实际文件准备及不可用标记仍由 Store 完成。
    pub fn fork_child_tasks(&self, prefix: &ConversationSnapshot) -> Vec<StoredChildTaskHistory> {
        let origins = self
            .message_origins
            .iter()
            .map(|origin| (&origin.message_id, origin.run_id.as_ref()))
            .collect::<HashMap<_, _>>();
        let mut calls = HashSet::new();
        let mut run = None;
        let mut selected = HashSet::new();
        for message in &prefix.messages {
            match message {
                ConversationMessage::Assistant(assistant) => {
                    calls.clear();
                    run = origins.get(&assistant.id).copied().flatten();
                    for part in &assistant.parts {
                        if let AssistantPart::ToolCall(call) = part
                            && call.name.as_str() == "delegate_task"
                        {
                            calls.insert(call.id.as_str());
                        }
                    }
                }
                ConversationMessage::Tool(tool) if calls.contains(tool.result.call_id.as_str()) => {
                    for part in tool.result.content.as_parts() {
                        let ToolResultPart::Json { value } = part else {
                            continue;
                        };
                        let Some(id) = value
                            .get("task_id")
                            .or_else(|| value.pointer("/error/details/task_id"))
                            .and_then(serde_json::Value::as_str)
                        else {
                            continue;
                        };
                        if let Some(child) = self.child_tasks.iter().find(|child| {
                            child.child_task_id.as_str() == id
                                && child.parent_tool_call_id.as_str()
                                    == tool.result.call_id.as_str()
                                && run.is_none_or(|run| run == &child.parent_run_id)
                                && child.status.is_some_and(|status| status.is_terminal())
                        }) {
                            selected.insert(child.child_task_id.clone());
                        }
                    }
                }
                ConversationMessage::Tool(_) => {}
                _ => {
                    calls.clear();
                    run = None;
                }
            }
        }
        self.child_tasks
            .iter()
            .filter(|child| selected.contains(&child.child_task_id))
            .cloned()
            .map(|mut child| {
                child.inherited = true;
                child.body_generation = 1;
                child
            })
            .collect()
    }
}

fn message_id(message: &ConversationMessage) -> &MessageId {
    match message {
        ConversationMessage::System(message) => &message.id,
        ConversationMessage::ContextSummary(message) => &message.id,
        ConversationMessage::User(message) => &message.id,
        ConversationMessage::Assistant(message) => &message.id,
        ConversationMessage::Tool(message) => &message.id,
    }
}

fn conflict(message: &'static str) -> StoreError {
    StoreError::new(StoreErrorKind::Conflict, message)
}
