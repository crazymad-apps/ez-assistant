use super::*;
use assistant_protocol::{FeedbackRequestSnapshot, FeedbackStatus};

impl VolatileRuntimeStore {
    pub(super) fn load_feedback(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, Vec<FeedbackRequestSnapshot>> {
        let session_id = session_id.clone();
        Box::pin(async move {
            Ok(self
                .lock()?
                .feedback_projection(&session_id)
                .into_values()
                .collect())
        })
    }
    pub(super) fn mutate_feedback(&self, mutation: crate::FeedbackMutation) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let request = &mutation.request;
            if !state.sessions.contains_key(&request.session_id) {
                return Err(conflict("feedback session missing"));
            }
            if state
                .runs
                .get(&request.run_id)
                .is_none_or(|run| run.session_id != request.session_id)
                && !state
                    .histories
                    .get(&request.session_id)
                    .is_some_and(|history| {
                        history
                            .runs
                            .iter()
                            .any(|run| run.run_id == request.run_id && !run.inherited)
                    })
            {
                return Err(conflict("feedback Run does not belong to Session"));
            }
            if state
                .feedback
                .get(&request.request_id)
                .is_some_and(|current| current.session_id != request.session_id)
            {
                return Err(conflict(
                    "feedback request control belongs to another Session",
                ));
            }
            validate_feedback_mutation(&state.feedback_projection(&request.session_id), &mutation)?;
            state.record_feedback_request(request.clone());
            Ok(())
        })
    }
}

impl State {
    /// 在 Store 锁内合并本 Session 的可靠问答与尚未回答控制，不安装历史执行对象。
    pub(super) fn feedback_projection(
        &self,
        session_id: &SessionId,
    ) -> BTreeMap<String, FeedbackRequestSnapshot> {
        let mut requests = self
            .histories
            .get(session_id)
            .into_iter()
            .flat_map(|history| history.questions.iter())
            .map(|request| (request.request_id.clone(), request.clone()))
            .collect::<BTreeMap<_, _>>();
        requests.extend(
            self.feedback
                .values()
                .filter(|request| &request.session_id == session_id)
                .map(|request| (request.request_id.clone(), request.clone())),
        );
        requests
    }

    /// 调用方已在同一独占提交校验状态转换；终态先冻结再释放控制，重复值仅覆盖同一历史身份。
    /// EndTurn 回答由接纳事务调用，队列正文仍保留在真实 Input 控制中，不随问题归档释放。
    pub(super) fn record_feedback_request(&mut self, request: FeedbackRequestSnapshot) {
        if request.status == FeedbackStatus::Pending {
            self.feedback.insert(request.request_id.clone(), request);
        } else {
            self.feedback.remove(&request.request_id);
            let history = self
                .histories
                .entry(request.session_id.clone())
                .or_default();
            history.questions.push(request);
            history.normalize();
        }
    }
}

pub(super) fn validate_feedback_mutation(
    requests: &BTreeMap<String, FeedbackRequestSnapshot>,
    mutation: &crate::FeedbackMutation,
) -> Result<(), StoreError> {
    let request = &mutation.request;
    match (requests.get(&request.request_id), mutation.expected_status) {
        (None, None)
            if request.status == FeedbackStatus::Pending
                && !requests.values().any(|r| {
                    r.session_id == request.session_id && r.status == FeedbackStatus::Pending
                }) =>
        {
            Ok(())
        }
        (Some(current), _) if current == request => Ok(()),
        (Some(current), Some(expected))
            if current.status == expected
                && current.session_id == request.session_id
                && current.run_id == request.run_id
                && current.tool_call_id == request.tool_call_id
                && current.mode == request.mode
                && current.questions == request.questions
                && expected == FeedbackStatus::Pending
                && request.status != FeedbackStatus::Pending =>
        {
            Ok(())
        }
        _ => Err(conflict("feedback already handled or changed")),
    }
}
