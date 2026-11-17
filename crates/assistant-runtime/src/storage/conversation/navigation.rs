//! 正文投影的只读缩略规则，与文件位置和执行对象无关。

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use agent_types::{AssistantPart, ConversationMessage, MessageId, UserMessage, UserPart};
use assistant_protocol::{ChildTaskStatus, ConversationNavigationItem, ConversationOwner, RunId};
use serde::{Deserialize, Serialize};

use crate::{
    StoreError, StoreErrorKind, StoredHistoryFacts, StoredMessageOrigin, StoredRunHistory,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NavigationBoundary {
    Before(usize),
    After(usize),
}

#[derive(Clone)]
pub enum ConversationNavigationRead {
    Page {
        boundary: Option<NavigationBoundary>,
        limit: usize,
    },
    Preview {
        message_id: MessageId,
    },
}

pub struct ConversationNavigationRequest {
    pub owner: ConversationOwner,
    pub generation: u64,
    pub read: ConversationNavigationRead,
    pub context: NavigationReplyContext,
}

/// 短暂观察值，由 Runtime 捕获及复核，只在本次读取中有效。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NavigationReplyContext {
    pub active_step: Option<(Option<RunId>, u32)>,
    pub child_status: Option<ChildTaskStatus>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum StoredConversationNavigation {
    Page {
        items: Vec<ConversationNavigationItem>,
        previous: Option<NavigationBoundary>,
        next: Option<NavigationBoundary>,
    },
    Preview(Option<(String, bool)>),
}

/// 冷扫描只收集可见行及选择回复所需的片段分类。
pub enum NavigationMessageKind {
    User,
    Assistant {
        has_text: bool,
        has_tools: bool,
        has_segments: bool,
    },
    Other,
}

impl NavigationMessageKind {
    pub fn classify(message: &ConversationMessage) -> Option<Self> {
        match message {
            ConversationMessage::User(user) if user.transcript_visibility.is_visible() => Some(
                if crate::shell::InheritedShellSwitchResult::encoded_result(user).is_some() {
                    Self::Other
                } else {
                    Self::User
                },
            ),
            ConversationMessage::Assistant(assistant) if assistant.purpose.is_model_response() => {
                let mut has_text = false;
                let mut has_tools = false;
                let mut has_reasoning = false;
                for part in &assistant.parts {
                    match part {
                        AssistantPart::Text(part) => has_text |= visible_text(&part.text),
                        AssistantPart::Reasoning(part) => has_reasoning |= visible_text(&part.text),
                        AssistantPart::ToolCall(_) => has_tools = true,
                        AssistantPart::ProviderState(_) => {}
                    }
                }
                Some(Self::Assistant {
                    has_text,
                    has_tools,
                    has_segments: has_text || has_tools || has_reasoning,
                })
            }
            ConversationMessage::ContextSummary(_) => Some(Self::Other),
            _ => None,
        }
    }
}

/// 复用可靠的消息归属、输入和控制结果投影。只在冷建或可靠追加时查这些事实。
pub struct NavigationHistory<'a> {
    origins: HashMap<&'a MessageId, &'a StoredMessageOrigin>,
    controls: HashSet<&'a MessageId>,
}

impl<'a> NavigationHistory<'a> {
    pub fn new(facts: &'a StoredHistoryFacts) -> Self {
        let shell_inputs: HashSet<_> = facts
            .inputs
            .iter()
            .filter(|input| input.agent_shell_target.is_some())
            .map(|input| &input.input_id)
            .collect();
        let shell_runs: HashSet<_> = facts
            .runs
            .iter()
            .filter(|run| {
                run.input_id
                    .as_ref()
                    .is_some_and(|id| shell_inputs.contains(id))
            })
            .map(|run| &run.run_id)
            .collect();
        let mut controls: HashSet<_> = facts
            .commands
            .iter()
            .filter(|command| command.result.is_some())
            .map(|command| &command.user_message_id)
            .collect();
        controls.extend(
            facts
                .message_origins
                .iter()
                .filter(|origin| match &origin.input_id {
                    Some(id) => shell_inputs.contains(id),
                    None => origin
                        .run_id
                        .as_ref()
                        .is_some_and(|id| shell_runs.contains(id)),
                })
                .map(|origin| &origin.message_id),
        );
        Self {
            origins: facts
                .message_origins
                .iter()
                .map(|origin| (&origin.message_id, origin))
                .collect(),
            controls,
        }
    }

    pub fn origin(&self, id: &MessageId) -> Option<&'a StoredMessageOrigin> {
        self.origins.get(id).copied()
    }

    pub fn controls(&self) -> impl Iterator<Item = &MessageId> {
        self.controls.iter().copied()
    }

    fn is_control(&self, id: &MessageId) -> bool {
        self.controls.contains(id)
    }
}

#[derive(Clone, Default)]
pub struct ConversationNavigationIndex {
    entries: Vec<Entry>,
    tail: Tail,
}

#[derive(Clone, Copy, Default)]
enum Tail {
    #[default]
    Other,
    User,
    Assistant,
}

#[derive(Clone)]
struct Entry {
    user_ordinal: usize,
    user_id: MessageId,
    reply: Option<ReplyCandidates>,
}

#[derive(Clone)]
struct ReplyCandidates {
    run_id: Option<RunId>,
    last_text: Option<MessageId>,
    // 与 presentTurn 一致，空片段不改变最后有内容的消息。
    provisional: Option<(MessageId, Option<u32>)>,
}

impl ConversationNavigationIndex {
    /// 按可见行的物理顺序追加。任一独立行或不同 Run 都结束前一输入的配对。
    pub fn push(
        &mut self,
        ordinal: usize,
        id: &MessageId,
        kind: NavigationMessageKind,
        history: &NavigationHistory<'_>,
        is_child: bool,
    ) {
        match kind {
            NavigationMessageKind::User if !history.is_control(id) => {
                self.entries.push(Entry {
                    user_ordinal: ordinal,
                    user_id: id.clone(),
                    reply: None,
                });
                self.tail = Tail::User;
            }
            NavigationMessageKind::Assistant {
                has_text,
                has_tools,
                has_segments,
            } => {
                let Some(entry) = self.entries.last_mut() else {
                    return;
                };
                let origin = history.origin(id);
                let run_id = origin.and_then(|origin| origin.run_id.as_ref());
                if matches!(self.tail, Tail::User) {
                    entry.reply = Some(ReplyCandidates {
                        run_id: run_id.cloned(),
                        last_text: None,
                        provisional: None,
                    });
                }
                let Some(reply) = entry.reply.as_mut().filter(|reply| match self.tail {
                    Tail::User => true,
                    Tail::Assistant => {
                        reply.run_id.as_ref() == run_id && (run_id.is_some() || is_child)
                    }
                    Tail::Other => false,
                }) else {
                    self.tail = Tail::Other;
                    return;
                };
                if has_text {
                    reply.last_text = Some(id.clone());
                }
                if has_segments {
                    reply.provisional = (has_text && !has_tools)
                        .then(|| (id.clone(), origin.and_then(|origin| origin.step)));
                }
                self.tail = Tail::Assistant;
            }
            NavigationMessageKind::User | NavigationMessageKind::Other => self.tail = Tail::Other,
        }
    }

    pub fn page<'a>(
        &self,
        boundary: Option<NavigationBoundary>,
        limit: usize,
        context: &NavigationReplyContext,
        run: impl Fn(&RunId) -> Option<&'a StoredRunHistory>,
    ) -> Result<StoredConversationNavigation, StoreError> {
        let total = self.entries.len();
        let invalid = || {
            StoreError::new(
                StoreErrorKind::InvalidInput,
                "invalid navigation boundary or limit",
            )
        };
        if limit == 0 {
            return Err(invalid());
        }
        let Range { start, end } = match boundary {
            None => total.saturating_sub(limit)..total,
            Some(NavigationBoundary::Before(end)) if end > 0 && end <= total => {
                end.saturating_sub(limit)..end
            }
            Some(NavigationBoundary::After(start)) if start < total => {
                start..start.saturating_add(limit).min(total)
            }
            _ => return Err(invalid()),
        };
        let items = self.entries[start..end]
            .iter()
            .map(|entry| {
                Ok(ConversationNavigationItem {
                    user_message_id: protocol_id(&entry.user_id)?,
                    assistant_message_id: entry
                        .reply
                        .as_ref()
                        .and_then(|reply| reply.select(context, &run))
                        .map(protocol_id)
                        .transpose()?,
                })
            })
            .collect::<Result<_, StoreError>>()?;
        Ok(StoredConversationNavigation::Page {
            items,
            previous: (start > 0).then_some(NavigationBoundary::Before(start)),
            next: (end < total).then_some(NavigationBoundary::After(end)),
        })
    }

    pub fn has_user_at(&self, ordinal: usize) -> bool {
        self.entries
            .binary_search_by_key(&ordinal, |entry| entry.user_ordinal)
            .is_ok()
    }

    /// 目标必须属于实际导航项，并且仍是这个输入当前可预览的助手消息。
    pub fn permits_preview<'a>(
        &self,
        ordinal: usize,
        id: &MessageId,
        context: &NavigationReplyContext,
        run: impl Fn(&RunId) -> Option<&'a StoredRunHistory>,
    ) -> bool {
        let next = self
            .entries
            .partition_point(|entry| entry.user_ordinal <= ordinal);
        let Some(entry) = next
            .checked_sub(1)
            .and_then(|index| self.entries.get(index))
        else {
            return false;
        };
        &entry.user_id == id
            || entry
                .reply
                .as_ref()
                .and_then(|reply| reply.select(context, &run))
                == Some(id)
    }
}

impl ReplyCandidates {
    fn select<'a>(
        &self,
        context: &NavigationReplyContext,
        run: impl Fn(&RunId) -> Option<&'a StoredRunHistory>,
    ) -> Option<&MessageId> {
        let terminal = if let Some(status) = context.child_status {
            status.is_terminal()
        } else {
            self.run_id.as_ref().and_then(run).is_some_and(|run| {
                !run.partial && run.status.is_some_and(|status| status.is_terminal())
            })
        };
        if terminal {
            return self.last_text.as_ref();
        }
        let (id, step) = self.provisional.as_ref()?;
        if context.active_step.as_ref().is_some_and(|(run, current)| {
            run == &self.run_id && step.is_none_or(|step| step < *current)
        }) {
            return None;
        }
        Some(id)
    }
}

fn protocol_id(id: &MessageId) -> Result<assistant_protocol::MessageId, StoreError> {
    assistant_protocol::MessageId::new(id.as_str()).map_err(|_| {
        StoreError::new(
            StoreErrorKind::InvalidData,
            "invalid conversation message id",
        )
    })
}

pub(crate) fn user_text_parts(message: &UserMessage) -> impl Iterator<Item = &str> {
    message.parts.iter().filter_map(|part| match part {
        UserPart::Text(part) => Some(part.text.as_str()),
        _ => None,
    })
}

/// 与 presentTurn 的 String.trim 对齐，避免空白片段成为预览候选。
fn visible_text(text: &str) -> bool {
    text.chars()
        .any(|ch| !(ch == '\u{feff}' || (ch != '\u{85}' && ch.is_whitespace())))
}

/// 先按正式正文片段次序拼接，再逐标量截取。只复制至多 160 个字符。
pub fn conversation_preview_text(message: &ConversationMessage) -> (String, bool) {
    const LIMIT: usize = 160;
    fn truncate<'a>(parts: impl Iterator<Item = &'a str>, separator: &str) -> (String, bool) {
        let mut text = String::new();
        let mut remaining = LIMIT;
        for (index, part) in parts.enumerate() {
            let prefix = if index == 0 { "" } else { separator };
            for ch in prefix.chars().chain(part.chars()) {
                if remaining == 0 {
                    return (text, true);
                }
                text.push(ch);
                remaining -= 1;
            }
        }
        (text, false)
    }
    match message {
        ConversationMessage::User(user) => truncate(user_text_parts(user), "\n"),
        ConversationMessage::Assistant(assistant) => truncate(
            assistant.parts.iter().filter_map(|part| match part {
                AssistantPart::Text(part) if visible_text(&part.text) => Some(part.text.as_str()),
                _ => None,
            }),
            "\n\n",
        ),
        _ => (String::new(), false),
    }
}
