//! Memory Demo 的单 Session、单活动 AgentExecution CLI 循环。

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use agent_core::{
    AgentEvent, AgentEventStream, AgentExecution, AllowAllAuthorizer, CompletionFuture,
    ExecutionContext, ExecutionControl, ExecutionInput, ExecutionOutcome, ExecutionRecorder,
};
use agent_memory::PinnedMemoryStore;
use agent_types::{ConversationMessage, MessageId, PartId, TextPart, UserMessage, UserPart};
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::{
    DemoError,
    input::{self, InputAction},
    journal::DemoJournal,
    resources::{ChatResources, SESSIONS_DIR, new_session_prompt},
    session::{continuation_session, create_new_session, restore_session, session_path},
};

static MESSAGE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionOrigin {
    Created,
    Restored,
}

impl std::fmt::Display for SessionOrigin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => formatter.write_str("created"),
            Self::Restored => formatter.write_str("restored"),
        }
    }
}

struct ChatSession {
    origin: SessionOrigin,
    journal: Arc<DemoJournal>,
}

struct ActiveRun {
    journal: Arc<DemoJournal>,
    events: AgentEventStream,
    completion: CompletionFuture,
    control: ExecutionControl,
    outcome: Option<ExecutionOutcome>,
    events_closed: bool,
}

impl ActiveRun {
    fn new(journal: Arc<DemoJournal>, execution: AgentExecution) -> Self {
        Self {
            journal,
            events: execution.events,
            completion: execution.completion,
            control: execution.control,
            outcome: None,
            events_closed: false,
        }
    }

    fn is_ready(&self) -> bool {
        self.outcome.is_some() && self.events_closed
    }
}

enum ChatSignal {
    Input(Option<input::InputResult>),
    Event(Option<AgentEvent>),
    Outcome(ExecutionOutcome),
}

pub(crate) async fn run_chat(
    data_dir: PathBuf,
    requested_session: Option<String>,
    resources: ChatResources,
) -> Result<(), DemoError> {
    let sessions_dir = data_dir.join(SESSIONS_DIR);
    let mut session = open_session(&sessions_dir, requested_session, &resources).await?;
    let initial = session.journal.record().await;
    println!(
        "Memory Demo session `{}` ({}) is ready. Use /state /new /cancel /quit.",
        initial.id, session.origin
    );
    let mut commands = input::spawn_stdin().map_err(|error| DemoError::Io(error.to_string()))?;
    let mut active: Option<ActiveRun> = None;
    let mut quit_requested = false;
    let mut accepting_input = true;
    let mut input_error: Option<io::Error> = None;

    loop {
        if active.as_ref().is_some_and(ActiveRun::is_ready) {
            let mut finished = active.take().ok_or_else(|| {
                DemoError::Execution("ready run disappeared before finalization".to_owned())
            })?;
            let outcome = finished.outcome.take().ok_or_else(|| {
                DemoError::Execution("ready run has no completion outcome".to_owned())
            })?;
            finish_run(&finished.journal, outcome).await?;
            if quit_requested {
                if let Some(error) = input_error {
                    return Err(DemoError::Io(error.to_string()));
                }
                break;
            }
            continue;
        }

        let Some(running) = active.as_mut() else {
            let Some(result) = commands.recv().await else {
                break;
            };
            let command = result.map_err(|error| DemoError::Io(error.to_string()))?;
            match input::action_for(command, false) {
                InputAction::Start(text) => {
                    active = Some(start_run(&session, &resources, text).await?);
                }
                InputAction::ShowState => print_state(&session, &resources).await?,
                InputAction::New => {
                    session = create_session(&sessions_dir, &resources).await?;
                    let record = session.journal.record().await;
                    println!(
                        "created new session `{}` from the latest pinned Store",
                        record.id
                    );
                }
                InputAction::Quit => break,
                InputAction::Reject(message) => eprintln!("{message}"),
                InputAction::Cancel | InputAction::CancelAndQuit => {
                    return Err(DemoError::Execution(
                        "invalid idle command disposition".to_owned(),
                    ));
                }
            }
            continue;
        };

        let signal = tokio::select! {
            result = commands.recv(), if accepting_input => ChatSignal::Input(result),
            event = running.events.next(), if !running.events_closed => ChatSignal::Event(event),
            outcome = &mut running.completion, if running.outcome.is_none() => {
                ChatSignal::Outcome(outcome)
            }
        };
        match signal {
            ChatSignal::Input(Some(Ok(command))) => match input::action_for(command, true) {
                InputAction::ShowState => print_state(&session, &resources).await?,
                InputAction::Cancel => {
                    running.control.cancel();
                    println!("cancel requested");
                }
                InputAction::CancelAndQuit => {
                    running.control.cancel();
                    quit_requested = true;
                    accepting_input = false;
                    println!("cancel requested; waiting for Agent cleanup before exit");
                }
                InputAction::Reject(message) => eprintln!("{message}"),
                InputAction::Start(_) | InputAction::New | InputAction::Quit => {
                    return Err(DemoError::Execution(
                        "invalid active command disposition".to_owned(),
                    ));
                }
            },
            ChatSignal::Input(Some(Err(error))) => {
                running.control.cancel();
                quit_requested = true;
                accepting_input = false;
                input_error = Some(error);
            }
            ChatSignal::Input(None) => {
                running.control.cancel();
                quit_requested = true;
                accepting_input = false;
            }
            ChatSignal::Event(Some(event)) => print_event(&event)?,
            ChatSignal::Event(None) => running.events_closed = true,
            ChatSignal::Outcome(outcome) => running.outcome = Some(outcome),
        }
    }
    Ok(())
}

async fn open_session(
    sessions_dir: &Path,
    requested_session: Option<String>,
    resources: &ChatResources,
) -> Result<ChatSession, DemoError> {
    match requested_session {
        Some(id) => {
            let record = restore_session(sessions_dir, &id)
                .await
                .map_err(|error| DemoError::Session(error.to_string()))?;
            let journal = Arc::new(
                DemoJournal::new(session_path(sessions_dir, &id), record)
                    .map_err(|error| DemoError::Session(error.to_string()))?,
            );
            if journal
                .recover_pending_exchange()
                .await
                .map_err(|error| DemoError::Session(error.to_string()))?
            {
                eprintln!(
                    "recovered an interrupted pending tool exchange with explicit error results"
                );
            }
            Ok(ChatSession {
                origin: SessionOrigin::Restored,
                journal,
            })
        }
        None => create_session(sessions_dir, resources).await,
    }
}

async fn create_session(
    sessions_dir: &Path,
    resources: &ChatResources,
) -> Result<ChatSession, DemoError> {
    let id = next_session_id();
    let store: Arc<dyn PinnedMemoryStore> = resources.memory.store.clone();
    let record = create_new_session(
        sessions_dir,
        new_session_prompt(id.clone()),
        store,
        &resources.memory.limits,
    )
    .await
    .map_err(|error| DemoError::Session(error.to_string()))?;
    let journal = DemoJournal::new(session_path(sessions_dir, &id), record)
        .map_err(|error| DemoError::Session(error.to_string()))?;
    Ok(ChatSession {
        origin: SessionOrigin::Created,
        journal: Arc::new(journal),
    })
}

async fn start_run(
    session: &ChatSession,
    resources: &ChatResources,
    text: String,
) -> Result<ActiveRun, DemoError> {
    session
        .journal
        .append_message(ConversationMessage::User(user_message(text)?))
        .await
        .map_err(|error| DemoError::Session(error.to_string()))?;
    let record = session.journal.record().await;
    let continuation =
        continuation_session(&record).map_err(|error| DemoError::Session(error.to_string()))?;
    let recorder: Arc<dyn ExecutionRecorder> = session.journal.clone();
    let execution = AgentExecution::start(
        resources.spec(continuation.system_prompt),
        ExecutionInput {
            conversation: continuation.conversation,
        },
        ExecutionContext {
            cancellation: CancellationToken::new(),
            recorder,
            authorizer: Arc::new(AllowAllAuthorizer),
        },
    );
    Ok(ActiveRun::new(Arc::clone(&session.journal), execution))
}

async fn finish_run(
    journal: &Arc<DemoJournal>,
    outcome: ExecutionOutcome,
) -> Result<(), DemoError> {
    match outcome {
        ExecutionOutcome::Completed(message) => {
            journal
                .append_message(ConversationMessage::Assistant(message))
                .await
                .map_err(|error| DemoError::Session(error.to_string()))?;
            println!("\nrun completed");
        }
        ExecutionOutcome::Failed(error) => eprintln!("\nrun failed: {error}"),
        ExecutionOutcome::Cancelled => println!("\nrun cancelled"),
        ExecutionOutcome::CompactionRequired { reason, step } => eprintln!(
            "\nrun requires context compaction at step {step} ({reason:?}); this demo does not \
             implement Runtime compaction"
        ),
    }
    Ok(())
}

async fn print_state(session: &ChatSession, resources: &ChatResources) -> Result<(), DemoError> {
    let record = session.journal.record().await;
    let latest = resources
        .memory
        .store
        .list(CancellationToken::new())
        .await
        .map_err(|error| DemoError::Memory(error.to_string()))?;
    println!("session: {} ({})", record.id, session.origin);
    println!(
        "system prompt parts: {}",
        record.system_prompt.parts().len()
    );
    println!("frozen pinned snapshot:");
    println!(
        "{}",
        record
            .system_prompt
            .parts()
            .last()
            .map(String::as_str)
            .unwrap_or("<missing pinned snapshot>")
    );
    println!("latest pinned Store ({} entries):", latest.len());
    for entry in latest {
        println!(
            "- id={} category={} content={} attributes={}",
            entry.id,
            entry.category,
            entry.content,
            serde_json::to_string(&entry.attributes)
                .map_err(|error| DemoError::Memory(error.to_string()))?
        );
    }
    println!("note: latest Store and frozen session snapshot may intentionally differ");
    Ok(())
}

fn print_event(event: &AgentEvent) -> Result<(), DemoError> {
    match event {
        AgentEvent::StepStarted { step } => println!("\n[model step {step}]"),
        AgentEvent::ReasoningDelta { .. } => {}
        AgentEvent::TextDelta { delta, .. } => {
            print!("{delta}");
            io::stdout()
                .flush()
                .map_err(|error| DemoError::Io(error.to_string()))?;
        }
        AgentEvent::ToolProposed { call } => println!("\n{}", tool_proposed_label(call)),
        AgentEvent::ToolStarted { call_id } => println!("[tool started] {call_id}"),
        AgentEvent::ToolCompleted { call_id, status } => {
            println!("[tool completed] {call_id}: {status:?}")
        }
        AgentEvent::ExecutionFailed { error, .. } => eprintln!("[execution failed] {error}"),
        AgentEvent::ExecutionCancelled { .. } => println!("[execution cancelled]"),
        _ => {}
    }
    Ok(())
}

fn tool_proposed_label(call: &agent_types::ToolCall) -> String {
    // 参数可能包含完整记忆正文；默认活动日志只显示工具名和调用 ID。
    format!("[tool proposed] {} ({})", call.name, call.id)
}

fn user_message(text: String) -> Result<UserMessage, DemoError> {
    let suffix = unique_suffix();
    Ok(UserMessage {
        id: MessageId::new(format!("user_{suffix}"))
            .map_err(|error| DemoError::Session(error.to_string()))?,
        parts: vec![UserPart::Text(TextPart {
            id: PartId::new(format!("user_text_{suffix}"))
                .map_err(|error| DemoError::Session(error.to_string()))?,
            text,
        })],
    })
}

fn next_session_id() -> String {
    format!("session_{}", unique_suffix())
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = MESSAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{nanos}_{sequence}")
}

#[cfg(test)]
mod tests {
    use agent_context::ContextWindowEvaluator;
    use agent_model::ModelCapabilities;
    use agent_testkit::{ModelScript, ScriptedModelService, message_events};
    use agent_types::{
        FinishReason, ModelIdentity, ProviderId, ToolCall, ToolCallId, ToolName, ToolResultContent,
        ToolResultStatus,
    };
    use futures_util::StreamExt;

    use crate::resources::build_memory_resources;

    use super::*;

    #[test]
    fn generated_ids_are_session_safe_and_unique() {
        let left = next_session_id();
        let right = next_session_id();
        assert_ne!(left, right);
        assert!(
            left.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        );
    }

    #[test]
    fn tool_activity_label_does_not_print_memory_arguments() {
        let call = agent_types::ToolCall {
            id: agent_types::ToolCallId::new("call_1").expect("valid call id"),
            name: agent_types::ToolName::new("pin_memory").expect("valid tool name"),
            arguments: serde_json::json!({"content": "private text"}),
        };
        let label = tool_proposed_label(&call);
        assert!(label.contains("pin_memory"));
        assert!(!label.contains("private text"));
    }

    #[tokio::test]
    async fn scripted_agent_calls_all_five_memory_tools_and_keeps_prompt_frozen() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let turns = [
            tool_turn(
                "assistant_1",
                "call_1",
                "pin_memory",
                serde_json::json!({
                    "category": "preference",
                    "content": "Use dark mode",
                    "attributes": {"scope": "demo"}
                }),
            ),
            tool_turn(
                "assistant_2",
                "call_2",
                "list_pinned_memories",
                serde_json::json!({}),
            ),
            tool_turn(
                "assistant_3",
                "call_3",
                "update_pinned_memory",
                serde_json::json!({
                    "id": "pinned_0000000001",
                    "content": "Use light mode"
                }),
            ),
            tool_turn(
                "assistant_4",
                "call_4",
                "recall_memory",
                serde_json::json!({
                    "query": "memory",
                    "limit": 4,
                    "sources": ["demo_records", "failing_demo"]
                }),
            ),
            tool_turn(
                "assistant_5",
                "call_5",
                "unpin_memory",
                serde_json::json!({"id": "pinned_0000000001"}),
            ),
            text_turn("assistant_6", "Memory demo sequence completed."),
        ];
        let model = Arc::new(ScriptedModelService::new(
            ModelCapabilities {
                reasoning: false,
                tool_calls: true,
                streaming: true,
            },
            128_000,
            turns
                .iter()
                .map(|message| ModelScript::Events(message_events(message))),
        ));
        let resources = ChatResources {
            memory: build_memory_resources(directory.path())
                .await
                .expect("build memory resources"),
            model: model.clone(),
            context_window: Arc::new(
                ContextWindowEvaluator::new(1.0).expect("valid context threshold"),
            ),
            model_request: agent_core::ModelRequestConfig::default(),
        };
        let sessions = directory.path().join(SESSIONS_DIR);
        let session = create_session(&sessions, &resources)
            .await
            .expect("create session");
        let frozen_prompt = session.journal.record().await.system_prompt;
        let ActiveRun {
            mut events,
            completion,
            journal,
            ..
        } = start_run(
            &session,
            &resources,
            "Run the scripted memory test".to_owned(),
        )
        .await
        .expect("start run");
        let observed = events.by_ref().collect::<Vec<_>>().await;
        let outcome = completion.await;
        assert_eq!(
            observed.iter().filter(|event| event.is_terminal()).count(),
            1
        );
        assert_eq!(outcome, ExecutionOutcome::Completed(turns[5].clone()));
        finish_run(&journal, outcome)
            .await
            .expect("finish scripted run");

        assert!(
            resources
                .memory
                .store
                .list(CancellationToken::new())
                .await
                .expect("list final Store")
                .is_empty(),
            "recall must not automatically pin data"
        );
        let record = journal.record().await;
        record
            .conversation
            .validate_tool_exchange_pairs()
            .expect("tool exchanges remain paired");
        let recall_result = record.conversation.messages.iter().find_map(|message| {
            let ConversationMessage::Tool(message) = message else {
                return None;
            };
            (message.result.call_id.as_str() == "call_4").then_some(&message.result)
        });
        let recall_result = recall_result.expect("recall result");
        assert_eq!(recall_result.status, ToolResultStatus::Success);
        let ToolResultContent::Json(value) = &recall_result.content else {
            panic!("recall result must be structured JSON");
        };
        assert_eq!(value["failures"][0]["source_id"], "failing_demo");
        let requests = model.take_requests();
        assert_eq!(requests.len(), 6);
        assert!(
            requests
                .iter()
                .all(|request| request.system == frozen_prompt)
        );
        assert!(requests.iter().all(|request| request.tools.len() == 5));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn demo_control_cancels_run_with_one_terminal_event() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let final_message = text_turn("assistant_cancel", "must not complete");
        let model = Arc::new(ScriptedModelService::completing(
            ModelCapabilities {
                reasoning: false,
                tool_calls: true,
                streaming: true,
            },
            128_000,
            final_message,
        ));
        let resources = ChatResources {
            memory: build_memory_resources(directory.path())
                .await
                .expect("build memory resources"),
            model,
            context_window: Arc::new(
                ContextWindowEvaluator::new(1.0).expect("valid context threshold"),
            ),
            model_request: agent_core::ModelRequestConfig::default(),
        };
        let session = create_session(&directory.path().join(SESSIONS_DIR), &resources)
            .await
            .expect("create session");
        let ActiveRun {
            mut events,
            completion,
            control,
            journal,
            ..
        } = start_run(&session, &resources, "cancel this run".to_owned())
            .await
            .expect("start run");
        control.cancel();
        let observed = events.by_ref().collect::<Vec<_>>().await;
        let outcome = completion.await;
        assert_eq!(outcome, ExecutionOutcome::Cancelled);
        assert_eq!(
            observed.iter().filter(|event| event.is_terminal()).count(),
            1
        );
        assert!(journal.record().await.pending_exchange.is_none());
    }

    fn tool_turn(
        message_id: &str,
        call_id: &str,
        name: &str,
        arguments: serde_json::Value,
    ) -> agent_types::AssistantMessage {
        agent_types::AssistantMessage {
            id: MessageId::new(message_id).expect("valid message id"),
            model: model_identity(),
            parts: vec![agent_types::AssistantPart::ToolCall(ToolCall {
                id: ToolCallId::new(call_id).expect("valid call id"),
                name: ToolName::new(name).expect("valid tool name"),
                arguments,
            })],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }
    }

    fn text_turn(message_id: &str, text: &str) -> agent_types::AssistantMessage {
        agent_types::AssistantMessage {
            id: MessageId::new(message_id).expect("valid message id"),
            model: model_identity(),
            parts: vec![agent_types::AssistantPart::Text(TextPart {
                id: PartId::new(format!("text_{message_id}")).expect("valid part id"),
                text: text.to_owned(),
            })],
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    fn model_identity() -> ModelIdentity {
        ModelIdentity::new(
            ProviderId::new("test").expect("valid provider id"),
            "test-memory-model",
        )
    }
}
