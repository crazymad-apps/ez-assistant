use agent_types::{ContextUsageAdjustment, ConversationMessage, ConversationSnapshot, MessageId};
use assistant_protocol::{
    ChildTaskId, ConversationOwner, GoalId, IdempotencyKey, RunId, SessionId, WorkspaceId,
};

use super::{NewStoredInput, StoredGoal, StoredInput, StoredRun};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RewriteGoalEffect {
    pub expected_goal_id: GoalId,
    pub expected_generation: u64,
    pub goal: StoredGoal,
}

/// 历史重新输入所需的完整新正文和结构化关联。
#[derive(Clone, Debug, PartialEq)]
pub struct ConversationRewrite {
    pub session_id: SessionId,
    pub target_user_message_id: MessageId,
    pub conversation: ConversationSnapshot,
    pub input: NewStoredInput,
    pub goal_effect: Option<RewriteGoalEffect>,
    pub changed_at_ms: i64,
}

/// 自动压缩要原子替换的正文所有者；Run 与 child 都必须仍处于活动执行期。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextReplacementTarget {
    Run {
        session_id: SessionId,
        run_id: RunId,
    },
    ChildTask {
        session_id: SessionId,
        child_task_id: ChildTaskId,
    },
    /// 不依附业务 Run 的手动 Session 压缩。
    IdleSession {
        session_id: SessionId,
        expected_generation: u64,
        operation_id: IdempotencyKey,
        compacted_message_count: u64,
        retained_message_count: u64,
    },
}

/// 提交压缩后的 Agent 有效上下文；Store 把它合并进完整产品 Conversation 的新 generation。
#[derive(Clone, Debug, PartialEq)]
pub struct ContextReplacement {
    pub target: ContextReplacementTarget,
    pub conversation: ConversationSnapshot,
    pub changed_at_ms: i64,
}

/// Store 实际提交的 generation 边界；新 generation 可能跳过遗留孤儿文件。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextReplacementResult {
    pub source_generation: u64,
    pub result_generation: u64,
    /// 新 generation 中完整产品 Conversation 的物理消息数。
    ///
    /// 它可能大于压缩后的执行上下文消息数；Runtime 不能再用 Journal 长度覆盖该值。
    pub product_message_count: u64,
}

/// 从完整产品 Conversation 派生 Agent 当前应读取的有效上下文。
///
/// 文本摘要和可靠原生压缩都保留截断语义；本地模式不回放原生窗口，只使用其后事实。
/// 开头连续的 System prefix 始终保留。该投影不修改持久化正文。
pub fn execution_context_from_product_history(
    history: &ConversationSnapshot,
) -> ConversationSnapshot {
    let prefix_end = history
        .messages
        .iter()
        .take_while(|message| matches!(message, ConversationMessage::System(_)))
        .count();
    // 必须先定位可靠原生边界，再移除其不透明内容；反过来会重新展开已压缩原文。
    // 先收窄历史还使后续损坏文本摘要的回退不能越过这条边界。
    let native_end = history.messages.iter().rposition(|message| {
        matches!(message, ConversationMessage::Assistant(assistant)
            if is_native_compaction(message)
            && matches!(assistant.parts.as_slice(), [agent_types::AssistantPart::ProviderState(state)]
                if state.context_effect() == Some(agent_types::ProviderContextEffect::ReplacePriorContext)))
    }).map_or(prefix_end, |index| index + 1);
    let local = ConversationSnapshot::new(
        history.messages[..prefix_end]
            .iter()
            .chain(&history.messages[native_end..])
            .filter(|message| !is_native_compaction(message))
            .cloned()
            .collect(),
    );
    let history = &local;
    let summary_index = history
        .messages
        .iter()
        .rposition(|message| matches!(message, ConversationMessage::ContextSummary(_)))
        .unwrap_or(prefix_end);
    let mut messages = history.messages[..prefix_end].to_vec();
    if let Some(ConversationMessage::ContextSummary(summary)) = history.messages.get(summary_index)
        && let Some(reference) = &summary.retained_user_message_id
    {
        let matches: Vec<_> = history.messages[..summary_index]
            .iter()
            .filter(|message| {
                matches!(message,
                    ConversationMessage::User(user) if &user.id == reference
                )
            })
            .collect();
        let duplicated = history.messages[summary_index + 1..].iter().any(|message| {
            matches!(message,
                ConversationMessage::User(user) if &user.id == reference
            )
        });
        if matches.len() != 1 || duplicated {
            // 损坏引用不能激活；完整原文仍在，移除摘要边界后按原文重建。
            return ConversationSnapshot::new(
                history
                    .messages
                    .iter()
                    .filter(|message| !matches!(message, ConversationMessage::ContextSummary(_)))
                    .cloned()
                    .collect(),
            );
        }
        messages.push(history.messages[summary_index].clone());
        messages.push(matches[0].clone());
        messages.extend_from_slice(&history.messages[summary_index + 1..]);
    } else {
        messages.extend_from_slice(&history.messages[summary_index..]);
    }
    ConversationSnapshot::new(messages)
}

/// 截断历史后，把不再能解析的摘要 usage 减法收敛为显式不可用。
///
/// Fork／改写可以保留 Context Summary 却移除它原先引用的 Assistant；新正文不能继续携带
/// 指向已不存在消息的近似边界。
pub fn normalize_context_usage_adjustments(conversation: &mut ConversationSnapshot) {
    for index in 0..conversation.messages.len() {
        let retained_assistant_id = match &conversation.messages[index] {
            ConversationMessage::ContextSummary(summary) => match &summary.usage_adjustment {
                Some(ContextUsageAdjustment::Subtract {
                    retained_assistant_id,
                    ..
                }) => Some(retained_assistant_id.clone()),
                None | Some(ContextUsageAdjustment::Unavailable) => None,
            },
            _ => None,
        };
        let Some(retained_assistant_id) = retained_assistant_id else {
            continue;
        };
        let reference_survives =
            conversation.messages[index + 1..]
                .iter()
                .any(|message| match message {
                    ConversationMessage::Assistant(assistant) => {
                        assistant.id == retained_assistant_id && assistant.usage.is_some()
                    }
                    _ => false,
                });
        if !reference_survives
            && let ConversationMessage::ContextSummary(summary) = &mut conversation.messages[index]
        {
            summary.usage_adjustment = Some(ContextUsageAdjustment::Unavailable);
        }
    }
}

/// 把一次压缩后的执行上下文合并回唯一、完整的产品 Conversation。
///
/// replacement 必须是 `System prefix → 新 Context Summary → 当前完整历史的原样尾部 → 可选恢复消息`。
/// 合并时复用产品历史开头的 System prefix，只把新摘要插入保留尾部之前，因此保留全部旧
/// 轮次且不会复制稳定消息 ID。
pub fn merge_context_replacement_with_product_history(
    history: &ConversationSnapshot,
    replacement: &ConversationSnapshot,
) -> Result<ConversationSnapshot, super::StoreError> {
    let replacement_prefix_end = replacement
        .messages
        .iter()
        .take_while(|message| matches!(message, ConversationMessage::System(_)))
        .count();
    if let Some(ConversationMessage::Assistant(boundary)) =
        replacement.messages.get(replacement_prefix_end)
        && boundary.purpose == agent_types::AssistantMessagePurpose::ContextCompaction
    {
        let history_prefix = history
            .messages
            .iter()
            .take_while(|m| matches!(m, ConversationMessage::System(_)))
            .count();
        let valid_state = matches!(boundary.parts.as_slice(), [agent_types::AssistantPart::ProviderState(state)]
            if state.context_effect() == Some(agent_types::ProviderContextEffect::ReplacePriorContext));
        if !valid_state
            || replacement.messages.len() != replacement_prefix_end + 1
            || replacement.messages[..replacement_prefix_end] != history.messages[..history_prefix]
            || history
                .messages
                .iter()
                .any(|message| message_id_for_replacement(message) == &boundary.id)
        {
            return Err(super::StoreError::new(
                super::StoreErrorKind::InvalidInput,
                "invalid native context replacement",
            ));
        }
        history.validate_tool_exchange_pairs().map_err(|source| {
            super::StoreError::with_source(
                super::StoreErrorKind::InvalidInput,
                "native replacement source has pending tools",
                source,
            )
        })?;
        let mut merged = history.clone();
        merged
            .messages
            .push(ConversationMessage::Assistant(boundary.clone()));
        merged.validate_tool_exchange_pairs().map_err(|source| {
            super::StoreError::with_source(
                super::StoreErrorKind::InvalidInput,
                "native replacement boundary is invalid",
                source,
            )
        })?;
        return Ok(merged);
    }
    if !matches!(
        replacement.messages.get(replacement_prefix_end),
        Some(ConversationMessage::ContextSummary(_))
    ) {
        return Err(super::StoreError::new(
            super::StoreErrorKind::InvalidInput,
            "context replacement must contain a context summary after its system prefix",
        ));
    }
    let history_prefix_end = history
        .messages
        .iter()
        .take_while(|message| matches!(message, ConversationMessage::System(_)))
        .count();
    if replacement.messages[..replacement_prefix_end] != history.messages[..history_prefix_end] {
        return Err(super::StoreError::new(
            super::StoreErrorKind::InvalidInput,
            "context replacement changed the protected system prefix",
        ));
    }
    let mut retained = &replacement.messages[replacement_prefix_end + 1..];
    let Some(ConversationMessage::ContextSummary(summary)) =
        replacement.messages.get(replacement_prefix_end)
    else {
        unreachable!();
    };
    let expanded_user = if let Some(reference) = &summary.retained_user_message_id {
        let matching: Vec<_> = history
            .messages
            .iter()
            .filter(|message| {
                matches!(message,
                    ConversationMessage::User(user) if &user.id == reference
                )
            })
            .collect();
        if matching.len() != 1 || retained.first() != Some(matching[0])
            || retained[1..].iter().any(|message| matches!(message, ConversationMessage::User(user) if &user.id == reference))
        {
            return Err(super::StoreError::new(super::StoreErrorKind::InvalidInput, "context replacement contains an invalid original user reference"));
        }
        retained = &retained[1..];
        true
    } else {
        false
    };
    // 只允许一条新建的隐藏能力上下文；不允许普通正文借压缩被修改或追加。
    if let Some(ConversationMessage::User(user)) = retained.last()
        && !history.messages.iter().any(|message| match message {
            ConversationMessage::User(message) => message.id == user.id,
            ConversationMessage::Assistant(message) => message.id == user.id,
            ConversationMessage::Tool(message) => message.id == user.id,
            ConversationMessage::System(message) => message.id == user.id,
            ConversationMessage::ContextSummary(message) => message.id == user.id,
        })
        && user.origin == agent_types::UserMessageOrigin::Runtime
        && user.transcript_visibility == agent_types::TranscriptVisibility::Hidden
        && !user.parts.is_empty()
        && user.parts.len() <= 2
        && user.parts.iter().all(|part| {
            matches!(part,
            agent_types::UserPart::InternalContext(part) if matches!(part.kind.as_str(),
                "mcp_server_directory" | "mcp_server_selection" | "shell_environment"))
        })
    {
        retained = &retained[..retained.len() - 1];
    }
    let effective: Vec<_> = history
        .messages
        .iter()
        .enumerate()
        .filter(|(_, message)| !is_native_compaction(message))
        .collect();
    if effective.len() < retained.len()
        || !effective[effective.len() - retained.len()..]
            .iter()
            .map(|(_, message)| *message)
            .eq(retained.iter())
    {
        return Err(super::StoreError::new(
            super::StoreErrorKind::InvalidInput,
            "context replacement does not retain an exact conversation suffix",
        ));
    }
    let prefix_len = effective
        .get(effective.len() - retained.len())
        .map_or(history.messages.len(), |(index, _)| *index);
    let replacement_body = &replacement.messages[replacement_prefix_end..];
    let mut messages = Vec::with_capacity(prefix_len + replacement_body.len());
    messages.extend_from_slice(&history.messages[..prefix_len]);
    messages.push(replacement_body[0].clone());
    // 原始尾部（含旧原生记录）保持一次且顺序不变；replacement 只增加摘要及可选恢复消息。
    messages.extend_from_slice(&history.messages[prefix_len..]);
    let restored_start = 1 + usize::from(expanded_user) + retained.len();
    messages.extend_from_slice(&replacement_body[restored_start..]);
    let merged = ConversationSnapshot::new(messages);
    merged.validate_tool_exchange_pairs().map_err(|source| {
        super::StoreError::with_source(
            super::StoreErrorKind::InvalidInput,
            "merged product conversation is invalid",
            source,
        )
    })?;
    Ok(merged)
}

fn is_native_compaction(message: &ConversationMessage) -> bool {
    matches!(message, ConversationMessage::Assistant(message)
        if message.purpose == agent_types::AssistantMessagePurpose::ContextCompaction)
}

fn message_id_for_replacement(message: &ConversationMessage) -> &MessageId {
    match message {
        ConversationMessage::System(m) => &m.id,
        ConversationMessage::User(m) => &m.id,
        ConversationMessage::Assistant(m) => &m.id,
        ConversationMessage::Tool(m) => &m.id,
        ConversationMessage::ContextSummary(m) => &m.id,
    }
}

/// generation 切换成功后创建的新 Input 与首次 Run。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RewriteResult {
    pub input: StoredInput,
    pub run: StoredRun,
    pub body_generation: u64,
}

/// Runtime 向 Store 请求的一段规范 Conversation 窗口；`end` 使用可显示消息序号。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationWindowRequest {
    pub owner: ConversationOwner,
    pub generation: u64,
    pub end: Option<usize>,
    pub limit: usize,
}

/// Store 基于可重建索引返回的规范消息窗口；offset 不进入产品协议。
#[derive(Clone, Debug, PartialEq)]
pub struct StoredConversationWindow {
    pub generation: u64,
    pub start: usize,
    pub end: usize,
    pub total: usize,
    pub conversation: ConversationSnapshot,
}

/// Runtime 内部以权威 JSONL 原始消息序号读取的有限窗口。
///
/// 该端口供 Conversation Recall 定位及子任务用量的同代历史读取；原始序号不进入产品协议。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationRawWindowRequest {
    pub owner: ConversationOwner,
    pub generation: u64,
    pub start: usize,
    pub limit: usize,
}

/// Store 返回的原始消息窗口；Recall 调用方继续按收录规则过滤可见正文。
#[derive(Clone, Debug, PartialEq)]
pub struct StoredConversationRawWindow {
    pub generation: u64,
    pub start: usize,
    pub end: usize,
    pub total: usize,
    pub conversation: ConversationSnapshot,
}

/// Runtime 请求按稳定 Message ID 在当前权威 generation 中重新定位消息。
///
/// 该操作只用于校验旧 Recall 引用；Store 不依赖引用携带的旧 ordinal 或 generation。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationMessageLocationRequest {
    pub owner: ConversationOwner,
    pub message_id: MessageId,
}

/// 当前权威 Conversation 中一条消息的位置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredConversationMessageLocation {
    pub generation: u64,
    /// JSONL 中包含工具与系统消息的原始序号，供 Recall 读取原始窗口。
    pub message_ordinal: u64,
    /// User/Assistant 可展示消息中的零基序号；非展示消息没有该位置。
    pub display_ordinal: Option<u64>,
}

#[cfg(test)]
mod tests {
    use agent_types::{
        AssistantMessage, ContextSummaryMessage, FinishReason, ModelIdentity, ProviderId,
        SystemMessage, TokenUsage, UserMessage,
    };

    use super::*;

    fn message_id(value: &str) -> MessageId {
        MessageId::new(value).expect("message id")
    }

    fn system(value: &str) -> ConversationMessage {
        ConversationMessage::System(SystemMessage {
            id: message_id(value),
            text: value.to_owned(),
        })
    }

    fn user(value: &str) -> ConversationMessage {
        ConversationMessage::User(UserMessage {
            id: message_id(value),
            origin: Default::default(),
            transcript_visibility: Default::default(),
            parts: Vec::new(),
        })
    }

    fn summary(value: &str) -> ConversationMessage {
        ConversationMessage::ContextSummary(ContextSummaryMessage {
            retained_user_message_id: None,
            id: message_id(value),
            text: value.to_owned(),
            model: None,
            usage: None,
            compacted_usage: None,
            usage_adjustment: None,
            programmatic_context: None,
        })
    }

    fn assistant_with_usage(value: &str) -> ConversationMessage {
        ConversationMessage::Assistant(AssistantMessage {
            purpose: agent_types::AssistantMessagePurpose::ModelResponse,
            id: message_id(value),
            model: ModelIdentity::new(ProviderId::new("test").unwrap(), "model"),
            parts: Vec::new(),
            finish_reason: FinishReason::Stop,
            usage: Some(TokenUsage {
                input_tokens: 8,
                output_tokens: 2,
                total_tokens: 10,
                cached_input_tokens: None,
                reasoning_tokens: None,
            }),
        })
    }

    fn native_boundary(value: &str) -> ConversationMessage {
        let ConversationMessage::Assistant(mut message) = assistant_with_usage(value) else {
            unreachable!()
        };
        message.purpose = agent_types::AssistantMessagePurpose::ContextCompaction;
        message.parts = vec![agent_types::AssistantPart::ProviderState(
            agent_types::OpaqueProviderState::new_routed(
                ProviderId::new("test").unwrap(),
                agent_types::ProtocolId::new("responses").unwrap(),
                "responses.compacted_window",
                "application/json",
                1,
                None,
                "a".repeat(64),
                b"{}".to_vec(),
            )
            .unwrap()
            .with_context_effect(agent_types::ProviderContextEffect::ReplacePriorContext)
            .unwrap(),
        )];
        ConversationMessage::Assistant(message)
    }

    #[test]
    fn local_mode_keeps_latest_native_cutoff_instead_of_reopening_older_history() {
        for earlier_summary in [false, true] {
            let mut messages = vec![system("system"), user("old")];
            if earlier_summary {
                messages.push(summary("old-summary"));
            }
            messages.extend([native_boundary("first-native"), user("between")]);
            messages.push(native_boundary("latest-native"));
            let mut history = ConversationSnapshot::new(messages);
            assert_eq!(
                execution_context_from_product_history(&history).messages,
                vec![system("system")]
            );
            history.messages.push(user("after"));
            let expected = ConversationSnapshot::new(vec![system("system"), user("after")]);
            assert_eq!(execution_context_from_product_history(&history), expected);
            let restored: ConversationSnapshot =
                serde_json::from_slice(&serde_json::to_vec(&history).unwrap()).unwrap();
            assert_eq!(execution_context_from_product_history(&restored), expected);
            assert_eq!(restored, history);
        }
    }

    #[test]
    fn invalid_local_summary_cannot_restore_a_user_before_the_native_cutoff() {
        let mut invalid_summary = summary("invalid-summary");
        let ConversationMessage::ContextSummary(value) = &mut invalid_summary else {
            unreachable!()
        };
        value.retained_user_message_id = Some(message_id("old"));
        let history = ConversationSnapshot::new(vec![
            system("system"),
            user("old"),
            native_boundary("native"),
            user("after"),
            invalid_summary,
            user("latest"),
        ]);
        assert_eq!(
            execution_context_from_product_history(&history).messages,
            vec![system("system"), user("after"), user("latest")]
        );
        // 仅有用途标记而没有可靠替代状态的记录不能成为截断依据。
        let mut incomplete = assistant_with_usage("incomplete");
        let ConversationMessage::Assistant(value) = &mut incomplete else {
            unreachable!()
        };
        value.purpose = agent_types::AssistantMessagePurpose::ContextCompaction;
        let history = ConversationSnapshot::new(vec![user("old"), incomplete, user("latest")]);
        assert_eq!(
            execution_context_from_product_history(&history).messages,
            vec![user("old"), user("latest")]
        );
    }

    #[test]
    fn local_mode_retains_tool_pairs_after_native_cutoff_without_old_call_ids() {
        let pair = |name: &str| {
            let ConversationMessage::Assistant(mut call) = assistant_with_usage(name) else {
                unreachable!()
            };
            call.parts = vec![agent_types::AssistantPart::ToolCall(
                agent_types::ToolCall {
                    id: agent_types::ToolCallId::new("same-call").unwrap(),
                    name: agent_types::ToolName::new("read_file").unwrap(),
                    arguments: serde_json::json!({}),
                },
            )];
            call.finish_reason = FinishReason::ToolCalls;
            [
                ConversationMessage::Assistant(call),
                ConversationMessage::Tool(agent_types::ToolMessage {
                    id: message_id(&format!("{name}-result")),
                    result: agent_types::ToolResult {
                        call_id: agent_types::ToolCallId::new("same-call").unwrap(),
                        status: agent_types::ToolResultStatus::Success,
                        content: agent_types::ToolResultContent::text(name),
                        metadata: None,
                    },
                }),
            ]
        };
        let mut messages = pair("old").to_vec();
        messages.push(native_boundary("native"));
        let retained = pair("new");
        messages.extend(retained.clone());
        let history = ConversationSnapshot::new(messages);
        history.validate_tool_exchange_pairs().unwrap();
        let projected = execution_context_from_product_history(&history);
        assert_eq!(projected.messages, retained);
        projected.validate_tool_exchange_pairs().unwrap();
    }

    #[test]
    fn repeated_compaction_preserves_product_history_and_moves_only_the_execution_boundary() {
        let prefix = system("system");
        let first = user("first");
        let second = user("second");
        let original =
            ConversationSnapshot::new(vec![prefix.clone(), first.clone(), second.clone()]);
        let first_replacement =
            ConversationSnapshot::new(vec![prefix.clone(), summary("summary-one"), second.clone()]);
        let first_product =
            merge_context_replacement_with_product_history(&original, &first_replacement)
                .expect("first compaction");
        assert_eq!(
            first_product.messages,
            vec![prefix.clone(), first, summary("summary-one"), second,]
        );
        assert_eq!(
            execution_context_from_product_history(&first_product),
            first_replacement
        );

        let third = user("third");
        let mut before_second = first_product.messages.clone();
        before_second.push(third.clone());
        let before_second = ConversationSnapshot::new(before_second);
        let second_replacement =
            ConversationSnapshot::new(vec![prefix.clone(), summary("summary-two"), third.clone()]);
        let second_product =
            merge_context_replacement_with_product_history(&before_second, &second_replacement)
                .expect("second compaction");
        assert_eq!(
            second_product.messages,
            vec![
                prefix.clone(),
                user("first"),
                summary("summary-one"),
                user("second"),
                summary("summary-two"),
                third,
            ]
        );
        assert_eq!(
            execution_context_from_product_history(&second_product),
            second_replacement
        );
    }

    #[test]
    fn compaction_restores_only_new_hidden_capability_context_and_preserves_the_suffix() {
        use crate::internal_boundary::{
            InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
        };
        let history = ConversationSnapshot::new(vec![user("first"), user("last")]);
        let restored = InternalBoundaryCoordinator::hidden_message(InternalBoundaryRequest {
            source: InternalBoundarySource::McpServerDirectory,
            text: "current directory".into(),
        })
        .unwrap()
        .0;
        let replacement = ConversationSnapshot::new(vec![
            summary("summary"),
            user("last"),
            ConversationMessage::User(restored.clone()),
        ]);
        let merged =
            merge_context_replacement_with_product_history(&history, &replacement).unwrap();
        assert_eq!(merged.messages[0], history.messages[0]);
        assert_eq!(execution_context_from_product_history(&merged), replacement);
        for invalid in [
            user("extra"),
            ConversationMessage::User(agent_types::UserMessage {
                id: message_id("first"),
                ..restored.clone()
            }),
            ConversationMessage::User(agent_types::UserMessage {
                transcript_visibility: agent_types::TranscriptVisibility::Visible,
                ..restored
            }),
        ] {
            let mut invalid_replacement = replacement.clone();
            *invalid_replacement.messages.last_mut().unwrap() = invalid;
            assert!(
                merge_context_replacement_with_product_history(&history, &invalid_replacement)
                    .is_err()
            );
        }
        let mut changed = replacement;
        changed.messages[1] = user("rewritten");
        assert!(merge_context_replacement_with_product_history(&history, &changed).is_err());
    }

    #[test]
    fn local_summary_after_native_mode_preserves_original_history_and_retained_tail() {
        let history = ConversationSnapshot::new(vec![
            user("first"),
            native_boundary("old-native"),
            user("between"),
            native_boundary("second-native"),
            user("after"),
            user("retained"),
        ]);
        assert_eq!(
            execution_context_from_product_history(&history).messages,
            vec![user("after"), user("retained")]
        );
        let replacement =
            ConversationSnapshot::new(vec![summary("new-local-summary"), user("retained")]);
        let merged =
            merge_context_replacement_with_product_history(&history, &replacement).unwrap();
        assert_eq!(execution_context_from_product_history(&merged), replacement);
        let originals: Vec<_> = merged
            .messages
            .iter()
            .filter(|message| !matches!(message, ConversationMessage::ContextSummary(_)))
            .cloned()
            .collect();
        assert_eq!(originals, history.messages);
    }

    #[test]
    fn truncation_marks_a_dangling_usage_adjustment_unavailable() {
        let retained = assistant_with_usage("retained");
        let mut context_summary = summary("summary");
        let ConversationMessage::ContextSummary(summary) = &mut context_summary else {
            unreachable!();
        };
        summary.usage_adjustment = Some(ContextUsageAdjustment::Subtract {
            retained_assistant_id: message_id("retained"),
            subtract_total_tokens: 5,
        });

        let mut survives = ConversationSnapshot::new(vec![context_summary.clone(), retained]);
        normalize_context_usage_adjustments(&mut survives);
        let ConversationMessage::ContextSummary(summary) = &survives.messages[0] else {
            unreachable!();
        };
        assert!(matches!(
            summary.usage_adjustment,
            Some(ContextUsageAdjustment::Subtract { .. })
        ));

        let mut truncated = ConversationSnapshot::new(vec![context_summary]);
        normalize_context_usage_adjustments(&mut truncated);
        let ConversationMessage::ContextSummary(summary) = &truncated.messages[0] else {
            unreachable!();
        };
        assert_eq!(
            summary.usage_adjustment,
            Some(ContextUsageAdjustment::Unavailable)
        );
    }

    #[test]
    fn intra_turn_references_restore_without_duplicate_product_users_and_reject_tampering() {
        let original_user = user("task");
        let first = assistant_with_usage("first");
        let last = assistant_with_usage("last");
        let history =
            ConversationSnapshot::new(vec![original_user.clone(), first.clone(), last.clone()]);
        let mut boundary = summary("summary-one");
        let ConversationMessage::ContextSummary(value) = &mut boundary else {
            unreachable!();
        };
        value.retained_user_message_id = Some(message_id("task"));
        let replacement =
            ConversationSnapshot::new(vec![boundary.clone(), original_user.clone(), last.clone()]);
        let merged =
            merge_context_replacement_with_product_history(&history, &replacement).unwrap();
        assert_eq!(
            merged.messages,
            vec![original_user.clone(), first, boundary.clone(), last.clone()]
        );
        assert_eq!(execution_context_from_product_history(&merged), replacement);
        let mut tampered = replacement.clone();
        let ConversationMessage::User(value) = &mut tampered.messages[1] else {
            unreachable!();
        };
        value.origin = agent_types::UserMessageOrigin::Runtime;
        assert!(merge_context_replacement_with_product_history(&history, &tampered).is_err());
        let mut duplicated = replacement.clone();
        duplicated.messages.push(original_user.clone());
        assert!(merge_context_replacement_with_product_history(&history, &duplicated).is_err());
        let mut missing = merged.clone();
        missing.messages.remove(0);
        assert_eq!(
            execution_context_from_product_history(&missing).messages,
            vec![assistant_with_usage("first"), last]
        );
        let mut second = replacement;
        let ConversationMessage::ContextSummary(value) = &mut second.messages[0] else {
            unreachable!();
        };
        value.id = message_id("summary-two");
        let twice = merge_context_replacement_with_product_history(&merged, &second).unwrap();
        assert_eq!(
            twice
                .messages
                .iter()
                .filter(|message| matches!(message, ConversationMessage::User(_)))
                .count(),
            1
        );
        assert_eq!(execution_context_from_product_history(&twice), second);
    }
}

/// Conversation Recall 的存储级检索范围；调用方权限仍由 Runtime 在进入 Store 前判定。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConversationSearchScope {
    /// 当前 Session 及其子任务 Conversation。
    Session { session_id: SessionId },
    /// 绑定到同一 Workspace 的全部 Session 及其子任务 Conversation。
    Workspace { workspace_id: WorkspaceId },
    /// Runtime 中全部未删除 Conversation。
    Global,
}

/// Runtime 向派生 Conversation 索引发起的一页检索请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationSearchRequest {
    pub query: String,
    pub scope: ConversationSearchScope,
    pub limit: usize,
}

/// 一条派生索引命中；正文仍须以对应 JSONL Conversation 为权威来源。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationSearchHit {
    pub child_task_title: Option<String>,
    pub owner: ConversationOwner,
    pub generation: u64,
    pub message_id: MessageId,
    pub message_ordinal: u64,
    pub created_at_ms: i64,
    pub text: String,
}

/// 一页派生检索结果。`partial` 表示仍有脏索引等待后续有界重建。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationSearchPage {
    pub hits: Vec<ConversationSearchHit>,
    pub partial: bool,
    pub failed_owners: Vec<ConversationOwner>,
}
