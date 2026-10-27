//! 与正文位置共用同一有界 LRU 的可靠元数据；不缓存历史消息正文。

use super::{codec::HistoryHeader, message_id};
use agent_types::ConversationSnapshot;
use assistant_protocol::{ChildTaskId, ConversationOwner, RunId};
use assistant_runtime::StoredHistoryFacts;
use std::collections::HashSet;

#[derive(Clone, Default)]
pub(super) struct HistoryOffsets {
    facts: StoredHistoryFacts,
    runs: HashSet<RunId>,
    children: HashSet<ChildTaskId>,
    inputs: HashSet<assistant_protocol::InputId>,
    attachments: HashSet<assistant_protocol::AttachmentId>,
}

impl HistoryOffsets {
    pub(super) fn deletion_counts(
        &self,
        runs: &HashSet<RunId>,
        children: &HashSet<ChildTaskId>,
        attachments: &HashSet<assistant_protocol::AttachmentId>,
    ) -> (usize, usize, usize) {
        (
            self.runs.union(runs).count(),
            self.children.union(children).count(),
            self.attachments.union(attachments).count(),
        )
    }

    pub(super) fn contains_run(&self, run: &RunId) -> bool {
        self.runs.contains(run)
    }
    pub(super) fn contains_input(&self, input: &assistant_protocol::InputId) -> bool {
        self.inputs.contains(input)
    }

    /// 冷扫描逐帧收集；EOF 或可靠追加后归并，未提交尾帧不能进入此索引。
    pub(super) fn record(&mut self, history: &StoredHistoryFacts) {
        self.runs
            .extend(history.runs.iter().map(|run| run.run_id.clone()));
        self.children.extend(
            history
                .child_tasks
                .iter()
                .map(|task| task.child_task_id.clone()),
        );
        self.inputs
            .extend(history.inputs.iter().map(|input| input.input_id.clone()));
        self.inputs.extend(
            history
                .commands
                .iter()
                .map(|command| command.input_id.clone()),
        );
        self.attachments.extend(
            history
                .attachments
                .iter()
                .map(|attachment| attachment.attachment_id.clone()),
        );
        self.facts.extend(history.clone());
    }
    pub(super) fn finish(&mut self) {
        self.facts.normalize();
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
    pub(super) fn run_origins(
        &self,
        runs: &HashSet<RunId>,
    ) -> Vec<assistant_runtime::StoredMessageOrigin> {
        self.facts
            .message_origins
            .iter()
            .filter(|origin| {
                !origin.inherited && origin.run_id.as_ref().is_some_and(|run| runs.contains(run))
            })
            .cloned()
            .collect()
    }
    pub(super) fn input(
        &self,
        id: &assistant_protocol::InputId,
    ) -> Option<assistant_runtime::StoredInputHistory> {
        self.facts
            .inputs
            .iter()
            .find(|input| &input.input_id == id)
            .cloned()
    }
    pub(super) fn snapshot(&self) -> StoredHistoryFacts {
        self.facts.clone()
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
