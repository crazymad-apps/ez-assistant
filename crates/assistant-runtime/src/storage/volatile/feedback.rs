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
                .feedback
                .values()
                .filter(|r| r.session_id == session_id)
                .cloned()
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
            {
                return Err(conflict("feedback Run does not belong to Session"));
            }
            validate_feedback_mutation(&state.feedback, &mutation)?;
            state
                .feedback
                .insert(request.request_id.clone(), request.clone());
            Ok(())
        })
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
