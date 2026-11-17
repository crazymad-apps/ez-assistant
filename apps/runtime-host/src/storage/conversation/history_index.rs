//! 与正文位置共用同一有界 LRU 的可靠元数据；不缓存历史消息正文。

use super::super::{StorageResult, invalid_data};
use super::{codec::HistoryHeader, message_id};
use agent_types::ConversationSnapshot;
use assistant_protocol::{ChildTaskId, ConversationOwner, IdempotencyKey, InputId, RunId};
use assistant_runtime::StoredHistoryFacts;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Default)]
pub(in crate::storage) struct HistoryOffsets {
    facts: StoredHistoryFacts,
    runs: HashMap<RunId, usize>,
    children: HashSet<ChildTaskId>,
    inputs: HashMap<InputId, usize>,
    commands: HashMap<InputId, usize>,
    request_keys: HashMap<IdempotencyKey, RequestKeyOwner>,
    first_runs: HashMap<InputId, usize>,
    run_inputs: HashSet<InputId>,
    origins: HashMap<RunId, Vec<usize>>,
    attachments: HashSet<assistant_protocol::AttachmentId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::storage) enum RequestKeyOwner {
    Message(InputId),
    Command(InputId),
}

impl HistoryOffsets {
    pub(super) fn deletion_counts(
        &self,
        runs: &HashSet<RunId>,
        children: &HashSet<ChildTaskId>,
        attachments: &HashSet<assistant_protocol::AttachmentId>,
    ) -> (usize, usize, usize) {
        (
            self.runs.len()
                + runs
                    .iter()
                    .filter(|run| !self.runs.contains_key(*run))
                    .count(),
            self.children.union(children).count(),
            self.attachments.union(attachments).count(),
        )
    }

    pub(super) fn contains_run(&self, run: &RunId) -> bool {
        self.runs.contains_key(run)
    }
    pub(in crate::storage) fn has_run_identity(&self, run: &RunId) -> bool {
        self.contains_run(run)
            || self
                .facts
                .child_tasks
                .iter()
                .any(|child| &child.parent_run_id == run)
    }
    pub(in crate::storage) fn has_run_for_input(&self, input: &InputId) -> bool {
        self.run_inputs.contains(input)
    }

    pub(super) fn contains_input(&self, input: &assistant_protocol::InputId) -> bool {
        self.inputs.contains_key(input)
            || self.commands.contains_key(input)
            || self.run_inputs.contains(input)
    }

    /// 冷扫描逐帧收集；EOF 或可靠追加后归并，未提交尾帧不能进入此索引。
    pub(super) fn record(&mut self, history: &StoredHistoryFacts) {
        self.children.extend(
            history
                .child_tasks
                .iter()
                .map(|task| task.child_task_id.clone()),
        );
        self.attachments.extend(
            history
                .attachments
                .iter()
                .map(|attachment| attachment.attachment_id.clone()),
        );
        self.facts.extend(history.clone());
    }
    pub(super) fn finish(&mut self) -> StorageResult<()> {
        self.facts.normalize();
        self.runs.clear();
        self.first_runs.clear();
        self.run_inputs.clear();
        self.inputs.clear();
        self.commands.clear();
        self.request_keys.clear();
        self.origins.clear();
        for (index, origin) in self.facts.message_origins.iter().enumerate() {
            self.run_inputs.extend(origin.input_id.iter().cloned());
            if !origin.inherited
                && let Some(run) = &origin.run_id
            {
                self.origins.entry(run.clone()).or_default().push(index);
            }
        }
        for (index, run) in self.facts.runs.iter().enumerate() {
            self.runs.insert(run.run_id.clone(), index);
            self.run_inputs.extend(run.input_id.iter().cloned());
            if run.attempt == 1
                && !run.inherited
                && let Some(input_id) = &run.input_id
                && self.first_runs.insert(input_id.clone(), index).is_some()
            {
                return Err(invalid_data("local Input has more than one first Run"));
            }
        }
        for (index, input) in self.facts.inputs.iter().enumerate() {
            self.inputs.insert(input.input_id.clone(), index);
            if let Some(key) = &input.idempotency_key
                && (input.inherited
                    || self
                        .request_keys
                        .insert(
                            key.clone(),
                            RequestKeyOwner::Message(input.input_id.clone()),
                        )
                        .is_some())
            {
                return Err(invalid_data("invalid message input request key in history"));
            }
        }
        for (index, command) in self.facts.commands.iter().enumerate() {
            self.commands.insert(command.input_id.clone(), index);
            if let Some(key) = &command.idempotency_key
                && self
                    .request_keys
                    .insert(
                        key.clone(),
                        RequestKeyOwner::Command(command.input_id.clone()),
                    )
                    .is_some()
            {
                return Err(invalid_data("duplicate command request key in history"));
            }
        }
        Ok(())
    }

    pub(super) fn run(&self, id: &RunId) -> Option<&assistant_runtime::StoredRunHistory> {
        self.runs
            .get(id)
            .and_then(|&index| self.facts.runs.get(index))
    }

    pub(super) fn children(
        &self,
        id: Option<&ChildTaskId>,
    ) -> Vec<assistant_runtime::StoredChildTaskHistory> {
        let mut children: Vec<_> = self
            .facts
            .child_tasks
            .iter()
            .filter(|task| id.is_none_or(|id| id == &task.child_task_id))
            .cloned()
            .collect();
        children.sort_by(|left, right| left.child_task_id.cmp(&right.child_task_id));
        children
    }
    pub(super) fn origins_for_run(
        &self,
        run: &RunId,
    ) -> Vec<assistant_runtime::StoredMessageOrigin> {
        self.origins
            .get(run)
            .into_iter()
            .flatten()
            .map(|&index| self.facts.message_origins[index].clone())
            .collect()
    }

    pub(super) fn run_origins(
        &self,
        runs: &HashSet<RunId>,
    ) -> Vec<assistant_runtime::StoredMessageOrigin> {
        runs.iter()
            .flat_map(|run| self.origins_for_run(run))
            .collect()
    }
    pub(in crate::storage) fn input(
        &self,
        id: &assistant_protocol::InputId,
    ) -> Option<assistant_runtime::StoredInputHistory> {
        self.inputs
            .get(id)
            .and_then(|&index| self.facts.inputs.get(index))
            .cloned()
    }
    pub(in crate::storage) fn command(
        &self,
        id: &InputId,
    ) -> Option<assistant_runtime::StoredSessionCommand> {
        self.commands
            .get(id)
            .and_then(|&index| self.facts.commands.get(index))
            .cloned()
    }
    pub(in crate::storage) fn request_key(&self, key: &IdempotencyKey) -> Option<RequestKeyOwner> {
        self.request_keys.get(key).cloned()
    }
    pub(in crate::storage) fn first_run(
        &self,
        id: &InputId,
    ) -> Option<assistant_runtime::StoredRunHistory> {
        self.first_runs
            .get(id)
            .and_then(|&index| self.facts.runs.get(index))
            .cloned()
    }
    pub(super) fn snapshot(&self) -> StoredHistoryFacts {
        self.facts.clone()
    }

    pub(in crate::storage) fn facts(&self) -> &StoredHistoryFacts {
        &self.facts
    }

    /// 分页只投影相应消息及其 Run 的最新可靠元数据；正文仍按位置读取。
    pub(super) fn window(
        &self,
        header: &HistoryHeader,
        conversation: &ConversationSnapshot,
    ) -> StoredHistoryFacts {
        let ids: HashSet<_> = conversation
            .messages
            .iter()
            .map(|message| message_id(message).as_str())
            .collect();
        let mut facts = self.facts.clone();
        facts
            .message_origins
            .retain(|origin| ids.contains(origin.message_id.as_str()));
        let runs: HashSet<_> = facts
            .message_origins
            .iter()
            .filter_map(|origin| origin.run_id.clone())
            .collect();
        facts.runs.retain(|run| runs.contains(&run.run_id));
        let inputs: HashSet<_> = facts
            .message_origins
            .iter()
            .filter_map(|origin| origin.input_id.clone())
            .chain(facts.runs.iter().filter_map(|run| run.input_id.clone()))
            .collect();
        facts.inputs.retain(|input| {
            inputs.contains(&input.input_id) || ids.contains(input.user_message_id.as_str())
        });
        facts.runs.retain(|run| runs.contains(&run.run_id));
        facts
            .message_origins
            .retain(|origin| ids.contains(origin.message_id.as_str()));
        facts
            .commands
            .retain(|command| ids.contains(command.user_message_id.as_str()));
        facts
            .skill_activations
            .retain(|activation| ids.contains(activation.message_id.as_str()));
        facts
            .mcp_selections
            .retain(|selection| ids.contains(selection.message_id.as_str()));
        facts
            .feedback
            .retain(|feedback| ids.contains(feedback.message_id.as_str()));
        facts.questions.clear();
        facts.attachments.clear();
        match &header.owner {
            ConversationOwner::ChildTask { child_task_id, .. } => facts
                .child_tasks
                .retain(|child| &child.child_task_id == child_task_id),
            ConversationOwner::MainSession { .. } => facts
                .child_tasks
                .retain(|child| runs.contains(&child.parent_run_id)),
        }
        facts
    }
}
