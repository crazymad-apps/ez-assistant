//! WorkPlan 的用户控制命令；Agent 更新仍由 `work_plan::UpdatePlanTool` 负责。

use assistant_protocol::{ClearWorkPlanRequest, ClearWorkPlanResult, RuntimeEvent};

use super::AssistantRuntime;
use crate::{RuntimeError, RuntimeResult, StoreErrorKind, WorkPlanClear};

impl AssistantRuntime {
    /// 以客户端观察到的 revision 清除工作计划；不改变 Goal、Conversation 或队列。
    pub async fn clear_work_plan(
        &self,
        request: ClearWorkPlanRequest,
    ) -> RuntimeResult<ClearWorkPlanResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        {
            let state = session.lock_state()?;
            let current = state
                .work_plan
                .as_ref()
                .ok_or(RuntimeError::InvalidRequest {
                    reason: "session has no work plan to clear",
                })?;
            if current.revision != request.expected_revision {
                return Err(RuntimeError::WorkPlanRevisionConflict {
                    session_id: request.session_id,
                });
            }
        }
        self.store
            .clear_work_plan(WorkPlanClear {
                session_id: request.session_id.clone(),
                expected_revision: request.expected_revision,
            })
            .await
            .map_err(|source| match source.kind() {
                StoreErrorKind::Conflict => RuntimeError::WorkPlanRevisionConflict {
                    session_id: request.session_id.clone(),
                },
                _ => RuntimeError::from_store("clear work plan", source),
            })?;
        session.lock_state()?.work_plan = None;
        self.publish(RuntimeEvent::WorkPlanChanged {
            session_id: request.session_id,
            revision: request.expected_revision,
        });
        Ok(ClearWorkPlanResult { work_plan: None })
    }
}
