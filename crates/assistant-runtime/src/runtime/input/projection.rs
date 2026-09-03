//! Store 已接受 Input/首次 Run 到 Session 执行 lane 的统一内存投影。

use assistant_protocol::{InputId, RunSnapshot};

use crate::{
    AcceptedInput, InputOrigin,
    run::RunRecord,
    session::{InputRecord, SessionState},
};

/// 一次新 Input 投影后需要向观察层公开的最小事实。
pub(crate) struct AcceptedInputProjection {
    pub(crate) input_id: InputId,
    pub(crate) run: RunSnapshot,
    /// 只有无 Goal binding 的 Session lane 会改变产品 Queue。
    pub(crate) queue_revision: Option<u64>,
}

/// 将 Store 已可靠接受的 Input/首次 Run 投影到唯一 Session lane。
///
/// 来源入口只负责准备和持久化自己的领域事实；Run、Input、Skill ledger 与 lane 的内存形态
/// 必须在这里保持一致，避免用户输入、主控投递和 Runtime continuation 各自复制一套入队逻辑。
pub(crate) fn project_accepted_input(
    state: &mut SessionState,
    accepted: AcceptedInput,
    mcp_selection: Option<crate::StoredMcpSelection>,
) -> AcceptedInputProjection {
    debug_assert!(
        !accepted.is_duplicate,
        "duplicate input must be resolved before session projection"
    );

    let input_id = accepted.input.input_id.clone();
    let run_id = accepted.run.run_id.clone();
    let record = RunRecord::accepted(&accepted.run, Vec::new());
    let run = record.snapshot();
    state.current_variant = accepted.input.agent_variant;

    let queue_revision = if accepted.input.goal_binding.is_some() {
        state.goal_inputs.push_back(input_id.clone());
        None
    } else if accepted.input.origin == InputOrigin::Runtime
        && accepted.input.goal_binding.is_none()
        && accepted.input.cross_session.is_none()
        && accepted.input.channel_source.is_none()
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

    if let Some(activation) = accepted.input.skill_activation.clone() {
        state.skill_activations.push(activation);
    }
    if let Some(selection) = mcp_selection {
        state.mcp_selections.push(selection);
    }
    let previous_run = state.runs.insert(run_id.clone(), record);
    let previous_input = state.inputs.insert(
        input_id.clone(),
        InputRecord {
            stored: accepted.input,
            first_run_id: run_id.clone(),
            latest_run_id: run_id,
        },
    );
    debug_assert!(
        previous_run.is_none() && previous_input.is_none(),
        "newly accepted input projection must not replace existing session facts"
    );

    AcceptedInputProjection {
        input_id,
        run,
        queue_revision,
    }
}
