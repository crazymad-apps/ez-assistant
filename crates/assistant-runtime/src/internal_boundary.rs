//! Runtime 内部上下文边界的唯一构造入口。

use agent_types::{
    ContextInsertionPayload, ContextInsertionPlan, InternalContextPart, TranscriptVisibility,
    UserMessage, UserMessageOrigin, UserPart,
};

use crate::{
    RuntimeError, RuntimeResult, id,
    run::{allocate_message_id, allocate_part_id},
};

/// Runtime 已知的内部上下文来源。
///
/// 该 enum 只负责消息构造的共性，不接管 Goal generation、WorkPlan revision 或其他
/// 领域状态机；新增来源必须显式选择 kind 和 retention key，不能在功能模块内直接
/// 拼装 `InternalContextPart`。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InternalBoundarySource {
    AgentVariant,
    SkillActivation,
    GoalStart,
    GoalResume,
    GoalContinuation,
    WorkPlan,
    DelegationContext,
    DelegationExpectedOutput,
    DelegationVariant,
    DelegationDirectories,
}

impl InternalBoundarySource {
    const fn kind(self) -> &'static str {
        match self {
            Self::AgentVariant => "agent_variant",
            Self::SkillActivation => "skill_activation",
            Self::GoalStart => "goal_start",
            Self::GoalResume => "goal_resume",
            Self::GoalContinuation => "goal_continuation",
            Self::WorkPlan => "work_plan",
            Self::DelegationContext => "delegation_context",
            Self::DelegationExpectedOutput => "delegation_expected_output",
            Self::DelegationVariant => "delegation_variant",
            Self::DelegationDirectories => "delegation_directories",
        }
    }
}

/// 功能模块交给统一边界构造器的冻结输入。
pub(crate) struct InternalBoundaryRequest {
    pub(crate) source: InternalBoundarySource,
    pub(crate) retention_key: Option<String>,
    pub(crate) text: String,
}

/// 规范 Part 与 Runtime boundary 的稳定关联身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InternalBoundaryIdentity {
    pub(crate) boundary_id: String,
    pub(crate) source: InternalBoundarySource,
}

/// 无状态协调器；权威落账仍由调用方所在的 Input、Goal 或 Tool Exchange 事务完成。
pub(crate) struct InternalBoundaryCoordinator;

impl InternalBoundaryCoordinator {
    /// 构造一份规范内部 Part 插入计划。
    pub(crate) fn plan(
        request: InternalBoundaryRequest,
    ) -> RuntimeResult<ContextInsertionPlan<InternalBoundaryIdentity>> {
        let boundary_id =
            id::generate("b").map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "internal boundary id random source",
            })?;
        let identity = InternalBoundaryIdentity {
            boundary_id: boundary_id.clone(),
            source: request.source,
        };
        let part = InternalContextPart::new(
            allocate_part_id()?,
            boundary_id,
            request.source.kind(),
            request.retention_key,
            request.text,
        )
        .map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "internal boundary context",
        })?;
        Ok(ContextInsertionPlan::canonical_internal(identity, part))
    }

    /// 将计划中的内部 Part 追加到既有规范 UserMessage。
    pub(crate) fn append(
        message: &mut UserMessage,
        request: InternalBoundaryRequest,
    ) -> RuntimeResult<InternalBoundaryIdentity> {
        let plan = Self::plan(request)?;
        let identity = plan.source_identity;
        let ContextInsertionPayload::InternalContext(part) = plan.payload else {
            return Err(RuntimeError::InternalStateUnavailable {
                component: "internal boundary payload",
            });
        };
        message.parts.push(UserPart::InternalContext(part));
        Ok(identity)
    }

    /// 在同一消息中指定来源之前插入内部 Part；找不到目标时退化为追加。
    ///
    /// 这用于 held Input 后补 Goal 上下文等场景，保证跨功能的内部消息顺序仍由统一
    /// 边界层维护，调用方不直接拆装 `InternalContextPart`。
    pub(crate) fn insert_before(
        message: &mut UserMessage,
        before: InternalBoundarySource,
        request: InternalBoundaryRequest,
    ) -> RuntimeResult<InternalBoundaryIdentity> {
        let plan = Self::plan(request)?;
        let identity = plan.source_identity;
        let ContextInsertionPayload::InternalContext(part) = plan.payload else {
            return Err(RuntimeError::InternalStateUnavailable {
                component: "internal boundary payload",
            });
        };
        let index = message
            .parts
            .iter()
            .position(|candidate| {
                matches!(candidate, UserPart::InternalContext(candidate) if candidate.kind == before.kind())
            })
            .unwrap_or(message.parts.len());
        message.parts.insert(index, UserPart::InternalContext(part));
        Ok(identity)
    }

    /// 创建只含一段内部上下文的隐藏 Runtime UserMessage。
    pub(crate) fn hidden_message(
        request: InternalBoundaryRequest,
    ) -> RuntimeResult<(UserMessage, InternalBoundaryIdentity)> {
        let plan = Self::plan(request)?;
        let identity = plan.source_identity;
        let ContextInsertionPayload::InternalContext(part) = plan.payload else {
            return Err(RuntimeError::InternalStateUnavailable {
                component: "internal boundary payload",
            });
        };
        Ok((
            UserMessage {
                id: allocate_message_id()?,
                origin: UserMessageOrigin::Runtime,
                transcript_visibility: TranscriptVisibility::Hidden,
                parts: vec![UserPart::InternalContext(part)],
            },
            identity,
        ))
    }
}

#[cfg(test)]
mod tests {
    use agent_types::{MessageId, TranscriptVisibility, UserMessageOrigin};

    use super::*;

    #[test]
    fn appending_and_hidden_messages_share_the_same_structured_boundary_shape() {
        let mut visible = UserMessage {
            id: MessageId::new("message-visible").expect("message id"),
            origin: UserMessageOrigin::User,
            transcript_visibility: TranscriptVisibility::Visible,
            parts: Vec::new(),
        };
        let visible_identity = InternalBoundaryCoordinator::append(
            &mut visible,
            InternalBoundaryRequest {
                source: InternalBoundarySource::WorkPlan,
                retention_key: Some("work_plan".to_owned()),
                text: "frozen plan".to_owned(),
            },
        )
        .expect("append boundary");
        let (hidden, hidden_identity) =
            InternalBoundaryCoordinator::hidden_message(InternalBoundaryRequest {
                source: InternalBoundarySource::GoalContinuation,
                retention_key: Some("goal:1".to_owned()),
                text: "continue goal".to_owned(),
            })
            .expect("hidden boundary");

        assert_ne!(visible_identity.boundary_id, hidden_identity.boundary_id);
        assert!(matches!(visible.parts[0], UserPart::InternalContext(_)));
        assert_eq!(hidden.origin, UserMessageOrigin::Runtime);
        assert_eq!(hidden.transcript_visibility, TranscriptVisibility::Hidden);
        assert!(matches!(hidden.parts[0], UserPart::InternalContext(_)));
    }

    #[test]
    fn inserting_a_late_goal_boundary_keeps_it_before_skill_activation() {
        let mut message = UserMessage {
            id: MessageId::new("message-held").expect("message id"),
            origin: UserMessageOrigin::User,
            transcript_visibility: TranscriptVisibility::Visible,
            parts: Vec::new(),
        };
        InternalBoundaryCoordinator::append(
            &mut message,
            InternalBoundaryRequest {
                source: InternalBoundarySource::SkillActivation,
                retention_key: Some("skill:review".to_owned()),
                text: "skill".to_owned(),
            },
        )
        .expect("skill boundary");
        InternalBoundaryCoordinator::insert_before(
            &mut message,
            InternalBoundarySource::SkillActivation,
            InternalBoundaryRequest {
                source: InternalBoundarySource::GoalResume,
                retention_key: Some("goal:1".to_owned()),
                text: "goal".to_owned(),
            },
        )
        .expect("Goal boundary");
        let kinds = message
            .parts
            .iter()
            .filter_map(|part| match part {
                UserPart::InternalContext(part) => Some(part.kind.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(kinds, vec!["goal_resume", "skill_activation"]);
    }
}
