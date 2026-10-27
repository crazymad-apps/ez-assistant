//! Store 已可靠暂存的 Input 到 Session 执行 lane 的统一内存投影。

use assistant_protocol::InputId;

use crate::{
    AcceptedInput, InputOrigin, RuntimeError, RuntimeResult,
    session::{InputRecord, SessionState},
};

/// 一次新 Input 投影后需要向观察层公开的最小事实。
pub(crate) struct AcceptedInputProjection {
    pub(crate) input_id: InputId,
    /// 只有无 Goal binding 的 Session lane 会改变产品 Queue。
    pub(crate) queue_revision: Option<u64>,
}

/// 将 Store 已可靠暂存的 Input 投影到唯一 Session lane。
///
/// 来源入口只负责准备和持久化自己的领域事实；Input、Skill ledger 与 lane 的内存形态
/// 必须在这里保持一致；不创建 Run，避免用户输入、主控投递和 Runtime continuation 复制入队生命周期。
pub(crate) fn project_accepted_input(
    state: &mut SessionState,
    accepted: AcceptedInput,
    mcp_selection: Option<crate::StoredMcpSelection>,
) -> RuntimeResult<AcceptedInputProjection> {
    let AcceptedInput::Control {
        input,
        run: None,
        is_duplicate: false,
    } = accepted
    else {
        return Err(RuntimeError::InternalStateUnavailable {
            component: "new input execution control",
        });
    };
    let input_id = input.input_id.clone();
    state.current_variant = input.agent_variant;

    let queue_revision = if input.goal_binding.is_some() {
        state.goal_inputs.push_back(input_id.clone());
        None
    } else if input.origin == InputOrigin::Runtime
        && input.goal_binding.is_none()
        && input.cross_session.is_none()
        && input.channel_source.is_none()
    {
        state.queue_item_ids.push_front(input_id.clone());
        None
    } else {
        state.queue_item_ids.push_back(input_id.clone());
        state.queue_revision = state.queue_revision.saturating_add(1);
        if state.goal.is_some() {
            state.resume_required = true;
        }
        Some(state.queue_revision)
    };

    if let Some(activation) = input.skill_activation.clone() {
        state.skill_activations.push(activation);
    }
    if let Some(selection) = mcp_selection {
        state.mcp_selections.push(selection);
    }
    let previous_input = state.inputs.insert(
        input_id.clone(),
        InputRecord {
            stored: *input,
            first_run_id: None,
            latest_run_id: None,
        },
    );
    debug_assert!(
        previous_input.is_none(),
        "newly accepted input projection must not replace existing session facts"
    );

    Ok(AcceptedInputProjection {
        input_id,
        queue_revision,
    })
}
