use std::sync::Arc;

use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};
use agent_types::ToolName;
use assistant_protocol::{
    FeedbackMode, FeedbackOption, FeedbackQuestion, FeedbackRequestSnapshot, FeedbackStatus, RunId,
    RuntimeEvent, ToolCallId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    RuntimeStore, feedback::FeedbackMutation, observation::ObservationCoordinator,
    session::SessionController,
};

#[derive(Clone, Copy, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mode {
    #[default]
    Wait,
    EndTurn,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct OptionInput {
    label: String,
    description: Option<String>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct QuestionInput {
    question: String,
    header: Option<String>,
    #[serde(default)]
    options: Vec<OptionInput>,
    #[serde(default)]
    multiple: bool,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AskUserInput {
    questions: Vec<QuestionInput>,
    #[serde(default)]
    mode: Mode,
}

pub(crate) struct FeedbackAuthorizationFacts;

pub(crate) struct AskUserTool {
    session: Arc<SessionController>,
    run_id: RunId,
    store: Arc<dyn RuntimeStore>,
    events: ObservationCoordinator,
}

impl AskUserTool {
    pub(crate) fn new(
        session: Arc<SessionController>,
        run_id: RunId,
        store: Arc<dyn RuntimeStore>,
        events: ObservationCoordinator,
    ) -> Self {
        Self {
            session,
            run_id,
            store,
            events,
        }
    }
}

impl Tool for AskUserTool {
    type Input = AskUserInput;
    type ResolvedInput = AskUserInput;
    type Output = Value;

    fn name(&self) -> ToolName {
        ToolName::new("ask_user").expect("static tool name")
    }
    fn description(&self) -> String {
        "Ask the user for missing information, preferences, or feedback needed to proceed. Prefer one complete question in the user's language; ask up to four related questions together. Omit options for text, or provide 2–4 concise choices. Users can always type their own answer and supplement selected choices. Set multiple=true only when several choices can apply. Do not add an Other option or invent answers. mode=wait (default): wait for answers as this tool's result, then continue. mode=end_turn: present questions and end this turn; the reply starts a new turn as a user message. Use end_turn when handing work back for later feedback. Call this tool alone, without other tool calls in the same response. Do not use it to bypass tool permissions or runtime approval.".into()
    }
    fn resolve(&self, input: AskUserInput) -> Result<ToolResolution<AskUserInput>, ToolError> {
        if !(1..=4).contains(&input.questions.len()) {
            return Err(ToolError::invalid_input(
                "questions: expected 1–4 questions",
            ));
        }
        for (index, question) in input.questions.iter().enumerate() {
            let fail =
                |detail: &str| ToolError::invalid_input(format!("questions[{index}].{detail}"));
            if question.question.trim().is_empty() || question.question.len() > 8192 {
                return Err(fail("question: expected nonempty text up to 8192 bytes"));
            }
            if question
                .header
                .as_ref()
                .is_some_and(|s| s.trim().is_empty() || s.chars().count() > 16)
            {
                return Err(fail("header: expected 1–16 characters"));
            }
            if !question.options.is_empty() && !(2..=4).contains(&question.options.len()) {
                return Err(fail("options: expected 2–4 choices or no choices"));
            }
            if question.multiple && question.options.is_empty() {
                return Err(fail("multiple: requires options"));
            }
            let mut labels = std::collections::BTreeSet::new();
            for (option_index, option) in question.options.iter().enumerate() {
                if option.label.trim().is_empty()
                    || option.label.len() > 512
                    || !labels.insert(option.label.trim())
                {
                    return Err(fail(&format!(
                        "options[{option_index}].label: expected a nonempty, unique label up to 512 bytes"
                    )));
                }
                if option.description.as_ref().is_some_and(|s| s.len() > 2048) {
                    return Err(fail(&format!(
                        "options[{option_index}].description: exceeds 2048 bytes"
                    )));
                }
            }
        }
        let semantic = serde_json::to_value(&input)
            .map_err(|_| ToolError::invalid_input("questions could not be encoded"))?;
        Ok(ToolResolution::with_facts(
            input,
            FeedbackAuthorizationFacts,
            semantic,
        ))
    }
    fn execute<'a>(
        &'a self,
        input: AskUserInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Value> {
        Box::pin(async move {
            let error = |_| ToolError::execution("feedback state could not be committed");
            let call_id = context
                .call_id()
                .ok_or_else(|| ToolError::execution("feedback call identity missing"))?;
            let request = FeedbackRequestSnapshot {
                request_id: crate::id::generate("feedback").map_err(error)?,
                session_id: self.session.id().clone(),
                run_id: self.run_id.clone(),
                tool_call_id: ToolCallId::new(call_id.as_str())
                    .map_err(|_| ToolError::execution("invalid feedback call identity"))?,
                mode: match input.mode {
                    Mode::Wait => FeedbackMode::Wait,
                    Mode::EndTurn => FeedbackMode::EndTurn,
                },
                questions: input
                    .questions
                    .into_iter()
                    .map(|q| FeedbackQuestion {
                        question: q.question,
                        header: q.header,
                        multiple: q.multiple,
                        options: q
                            .options
                            .into_iter()
                            .map(|o| FeedbackOption {
                                label: o.label,
                                description: o.description,
                            })
                            .collect(),
                    })
                    .collect(),
                status: FeedbackStatus::Pending,
                answers: None,
            };
            {
                let _mutation = self.session.mutation().await;
                if context.cancellation.is_cancelled() {
                    return Err(ToolError::execution("feedback cancelled"));
                }
                self.store
                    .mutate_feedback(FeedbackMutation {
                        request: request.clone(),
                        expected_status: None,
                    })
                    .await
                    .map_err(|_| ToolError::execution("feedback could not be saved"))?;
                self.session
                    .lock_state()
                    .map_err(|_| ToolError::execution("feedback state unavailable"))?
                    .feedback
                    .insert(request.request_id.clone(), request.clone());
                let _ = self.events.send(RuntimeEvent::FeedbackPublished {
                    request: request.clone(),
                });
            }
            if request.mode == FeedbackMode::EndTurn {
                return Ok(
                    json!({"status":"waiting_for_user", "message":"Questions presented. This turn ends now; the user's reply will arrive as a new user message."}),
                );
            }
            loop {
                // 先登记 Notify 再检查状态，避免回答落在检查与 await 之间而丢失唤醒。
                let changed = self.session.feedback_changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let current = self
                    .session
                    .lock_state()
                    .map_err(|_| ToolError::execution("feedback state unavailable"))?
                    .feedback
                    .get(&request.request_id)
                    .cloned()
                    .ok_or_else(|| ToolError::execution("feedback expired"))?;
                if current.status == FeedbackStatus::Answered {
                    let answers = current.answers.unwrap_or_default();
                    return Ok(
                        json!({"status":"answered", "answers": current.questions.iter().zip(answers).map(|(q,a)| json!({"question":q.question, "selected_options": a.selected_options.iter().map(|i| q.options[*i as usize].label.clone()).collect::<Vec<_>>(), "text":a.text})).collect::<Vec<_>>()}),
                    );
                }
                if current.status != FeedbackStatus::Pending || context.cancellation.is_cancelled()
                {
                    return Err(ToolError::execution("feedback cancelled"));
                }
                tokio::select! { _ = &mut changed => {}, _ = context.cancellation.cancelled() => { return Err(ToolError::execution("feedback cancelled")); } }
            }
        })
    }
}
