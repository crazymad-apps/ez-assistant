use std::{num::NonZeroU32, sync::Arc};

use agent_context::ContextWindowEvaluator;
use agent_core::{
    ActiveGuardrailMode, AgentEvent, ExecutionBudget, ExecutionContext, ExecutionInput,
    ExecutionOutcome, GuardrailCheckConfig, GuardrailConfig, ModelRequestConfig,
};
use agent_model::{
    GenerationConfig, ModelCapabilities, ProviderOptions, ReasoningConfig, ReasoningEffort,
    SystemPromptSnapshot,
};
use agent_sdk::{AgentBuildError, AgentBuilder, AllowAllAuthorizer};
use agent_testkit::{
    InMemoryRecorder, ModelScript, OrderLog, ScriptedModelService, ScriptedTool, message_events,
};
use agent_tools::{ToolRegistry, ToolSetSnapshot};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FinishReason,
    MessageId, ModelIdentity, PartId, ProviderId, TextPart, ToolCall, ToolCallId, ToolChoice,
    ToolName, UserMessage, UserPart,
};
use futures_util::StreamExt;
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const CONTEXT_WINDOW_TOKENS: u64 = 16_384;

fn capabilities(reasoning: bool, tool_calls: bool) -> ModelCapabilities {
    ModelCapabilities {
        image_input: false,
        reasoning,
        tool_calls,
        multimodal_tool_result: false,
        tool_choice: if tool_calls {
            agent_model::ToolChoiceCapabilities::all()
        } else {
            agent_model::ToolChoiceCapabilities::default()
        },
        streaming: true,
    }
}

fn evaluator() -> Arc<ContextWindowEvaluator> {
    Arc::new(ContextWindowEvaluator::new(0.8).expect("valid threshold"))
}

fn prompt() -> SystemPromptSnapshot {
    SystemPromptSnapshot::new(vec!["You are an offline fixture agent.".to_owned()])
}

fn input(id: &str, text: &str) -> ExecutionInput {
    ExecutionInput {
        conversation: ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
            id: MessageId::new(id).expect("valid message id"),
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new(format!("{id}_text")).expect("valid part id"),
                text: text.to_owned(),
            })],
        })]),
    }
}

fn identity() -> ModelIdentity {
    ModelIdentity::new(
        ProviderId::new("fixture").expect("valid provider id"),
        "fixture-model",
    )
}

fn text_message(id: &str, text: &str) -> AssistantMessage {
    AssistantMessage {
        id: MessageId::new(id).expect("valid message id"),
        model: identity(),
        parts: vec![AssistantPart::Text(TextPart {
            id: PartId::new(format!("{id}_text")).expect("valid part id"),
            text: text.to_owned(),
        })],
        finish_reason: FinishReason::Stop,
        usage: None,
    }
}

fn tool_call_message(id: &str, call_id: &str, tool: &str) -> AssistantMessage {
    AssistantMessage {
        id: MessageId::new(id).expect("valid message id"),
        model: identity(),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new(call_id).expect("valid call id"),
            name: ToolName::new(tool).expect("valid tool name"),
            arguments: json!({"value": 1}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    }
}

fn tool_snapshot(name: &str, log: OrderLog) -> ToolSetSnapshot {
    let mut registry = ToolRegistry::new();
    registry
        .register(ScriptedTool::succeed(name, json!({"ok": true}), log))
        .expect("register fixture tool");
    registry.snapshot()
}

fn expect_build_error(
    result: Result<agent_sdk::Agent, AgentBuildError>,
    context: &str,
) -> AgentBuildError {
    match result {
        Ok(_) => panic!("{context}"),
        Err(error) => error,
    }
}

async fn complete_ephemeral(
    agent: &agent_sdk::Agent,
    execution_input: ExecutionInput,
) -> ExecutionOutcome {
    agent
        .start_ephemeral(
            execution_input,
            CancellationToken::new(),
            Arc::new(AllowAllAuthorizer),
        )
        .completion
        .await
}

#[tokio::test]
async fn builder_defaults_are_core_defaults_and_empty_tools_are_valid() {
    let model = Arc::new(ScriptedModelService::completing(
        capabilities(false, false),
        CONTEXT_WINDOW_TOKENS,
        text_message("answer", "done"),
    ));
    let agent = AgentBuilder::new(model.clone(), prompt(), evaluator())
        .build()
        .expect("default agent builds");

    assert_eq!(agent.system_prompt(), &prompt());
    assert!(agent.tool_definitions().is_empty());
    assert!(matches!(
        complete_ephemeral(&agent, input("user_1", "hello")).await,
        ExecutionOutcome::Completed(_)
    ));
    let requests = model.take_requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].tools.is_empty());
    assert_eq!(requests[0].tool_choice, ToolChoice::Auto);
    assert_eq!(requests[0].generation, GenerationConfig::default());
    assert_eq!(requests[0].reasoning, None);
    assert!(requests[0].provider_options.is_empty());
}

#[test]
fn builder_reports_each_cross_field_error_without_model_call() {
    let zero_window = Arc::new(ScriptedModelService::new(capabilities(true, true), 0, []));
    assert_eq!(
        expect_build_error(
            AgentBuilder::new(zero_window.clone(), prompt(), evaluator()).build(),
            "zero context window must fail",
        ),
        AgentBuildError::ZeroContextWindow
    );
    assert!(zero_window.take_requests().is_empty());

    let no_tools_capability = Arc::new(ScriptedModelService::new(
        capabilities(false, false),
        CONTEXT_WINDOW_TOKENS,
        [],
    ));
    assert_eq!(
        expect_build_error(
            AgentBuilder::new(no_tools_capability.clone(), prompt(), evaluator())
                .tools(tool_snapshot("lookup", OrderLog::new()))
                .build(),
            "registered tools require capability",
        ),
        AgentBuildError::ToolCallsUnsupported
    );
    assert!(no_tools_capability.take_requests().is_empty());

    let required = Arc::new(ScriptedModelService::new(
        capabilities(false, true),
        CONTEXT_WINDOW_TOKENS,
        [],
    ));
    assert_eq!(
        expect_build_error(
            AgentBuilder::new(required.clone(), prompt(), evaluator())
                .model_request(ModelRequestConfig {
                    tool_choice: ToolChoice::Required,
                    ..ModelRequestConfig::default()
                })
                .build(),
            "required needs a registered tool",
        ),
        AgentBuildError::RequiredToolChoiceWithoutTools
    );
    assert!(required.take_requests().is_empty());

    let mut auto_only_capabilities = capabilities(false, true);
    auto_only_capabilities.tool_choice = agent_model::ToolChoiceCapabilities::auto_only();
    let auto_only = Arc::new(ScriptedModelService::new(
        auto_only_capabilities,
        CONTEXT_WINDOW_TOKENS,
        [],
    ));
    assert_eq!(
        expect_build_error(
            AgentBuilder::new(auto_only.clone(), prompt(), evaluator())
                .tools(tool_snapshot("lookup", OrderLog::new()))
                .model_request(ModelRequestConfig {
                    tool_choice: ToolChoice::Required,
                    ..ModelRequestConfig::default()
                })
                .build(),
            "required must be supported by the exact route",
        ),
        AgentBuildError::ToolChoiceUnsupported
    );
    assert!(auto_only.take_requests().is_empty());

    let named = Arc::new(ScriptedModelService::new(
        capabilities(false, true),
        CONTEXT_WINDOW_TOKENS,
        [],
    ));
    let missing_name = ToolName::new("missing").expect("valid tool name");
    assert_eq!(
        expect_build_error(
            AgentBuilder::new(named.clone(), prompt(), evaluator())
                .model_request(ModelRequestConfig {
                    tool_choice: ToolChoice::Named(missing_name.clone()),
                    ..ModelRequestConfig::default()
                })
                .build(),
            "named tool must be registered",
        ),
        AgentBuildError::NamedToolChoiceNotRegistered { name: missing_name }
    );
    assert!(named.take_requests().is_empty());

    let no_reasoning = Arc::new(ScriptedModelService::new(
        capabilities(false, false),
        CONTEXT_WINDOW_TOKENS,
        [],
    ));
    assert_eq!(
        expect_build_error(
            AgentBuilder::new(no_reasoning.clone(), prompt(), evaluator())
                .model_request(ModelRequestConfig {
                    reasoning: Some(ReasoningConfig { effort: None }),
                    ..ModelRequestConfig::default()
                })
                .build(),
            "reasoning requires capability",
        ),
        AgentBuildError::ReasoningUnsupported
    );
    assert!(no_reasoning.take_requests().is_empty());
}

#[tokio::test]
async fn agent_reuses_frozen_configuration_across_sequential_starts() {
    let log = OrderLog::new();
    let tools = tool_snapshot("lookup", log);
    let model = Arc::new(ScriptedModelService::new(
        capabilities(true, true),
        CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&text_message("answer_1", "first"))),
            ModelScript::Events(message_events(&text_message("answer_2", "second"))),
        ],
    ));
    let mut provider_options = ProviderOptions::new();
    provider_options
        .insert("fixture", json!({"mode": "strict"}))
        .expect("valid fixture options");
    let request_config = ModelRequestConfig {
        tool_choice: ToolChoice::None,
        generation: GenerationConfig {
            temperature: Some(0.1),
            top_p: Some(0.9),
            max_output_tokens: Some(256),
            stop: vec!["done".to_owned()],
        },
        reasoning: Some(ReasoningConfig {
            effort: Some(ReasoningEffort::Low),
        }),
        provider_options,
    };
    let agent = AgentBuilder::new(model.clone(), prompt(), evaluator())
        .tools(tools)
        .model_request(request_config.clone())
        .budget(ExecutionBudget::default())
        .guardrails(GuardrailConfig {
            repeated_invocation: Some(GuardrailCheckConfig {
                mode: ActiveGuardrailMode::Observe,
                threshold: NonZeroU32::new(3).expect("non-zero threshold"),
            }),
            consecutive_failures: None,
        })
        .build()
        .expect("valid configured agent");

    assert_eq!(agent.tool_definitions().len(), 1);
    assert_eq!(agent.tool_definitions()[0].name.as_str(), "lookup");
    assert!(matches!(
        complete_ephemeral(&agent, input("user_1", "first")).await,
        ExecutionOutcome::Completed(_)
    ));
    assert!(matches!(
        complete_ephemeral(&agent, input("user_2", "second")).await,
        ExecutionOutcome::Completed(_)
    ));

    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert_eq!(request.system, prompt());
        assert_eq!(request.tools, agent.tool_definitions());
        assert_eq!(request.tool_choice, request_config.tool_choice);
        assert_eq!(request.generation, request_config.generation);
        assert_eq!(request.reasoning, request_config.reasoning);
        assert_eq!(request.provider_options, request_config.provider_options);
    }
}

#[tokio::test]
async fn ephemeral_path_supports_single_tool_loop() {
    let log = OrderLog::new();
    let model = Arc::new(ScriptedModelService::new(
        capabilities(false, true),
        CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&tool_call_message(
                "tool_turn",
                "call_1",
                "lookup",
            ))),
            ModelScript::Events(message_events(&text_message("answer", "done"))),
        ],
    ));
    let agent = AgentBuilder::new(model.clone(), prompt(), evaluator())
        .tools(tool_snapshot("lookup", log))
        .build()
        .expect("valid tool agent");

    assert!(matches!(
        complete_ephemeral(&agent, input("user_1", "look once")).await,
        ExecutionOutcome::Completed(message)
            if message == text_message("answer", "done")
    ));
    assert_eq!(model.take_requests().len(), 2);
}

#[tokio::test]
async fn ephemeral_path_supports_multi_round_tool_loop() {
    let log = OrderLog::new();
    let model = Arc::new(ScriptedModelService::new(
        capabilities(false, true),
        CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&tool_call_message(
                "tool_turn_1",
                "call_1",
                "lookup",
            ))),
            ModelScript::Events(message_events(&tool_call_message(
                "tool_turn_2",
                "call_2",
                "lookup",
            ))),
            ModelScript::Events(message_events(&text_message("answer", "done"))),
        ],
    ));
    let agent = AgentBuilder::new(model.clone(), prompt(), evaluator())
        .tools(tool_snapshot("lookup", log))
        .build()
        .expect("valid tool agent");

    assert!(matches!(
        complete_ephemeral(&agent, input("user_1", "look twice")).await,
        ExecutionOutcome::Completed(message)
            if message == text_message("answer", "done")
    ));
    assert_eq!(model.take_requests().len(), 3);
}

#[tokio::test]
async fn explicit_context_replaces_ephemeral_recorder_and_records_exchange() {
    let log = OrderLog::new();
    let model = Arc::new(ScriptedModelService::new(
        capabilities(false, true),
        CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&tool_call_message(
                "tool_turn",
                "call_1",
                "lookup",
            ))),
            ModelScript::Events(message_events(&text_message("answer", "done"))),
        ],
    ));
    let agent = AgentBuilder::new(model, prompt(), evaluator())
        .tools(tool_snapshot("lookup", log.clone()))
        .build()
        .expect("valid tool agent");
    let recorder = Arc::new(InMemoryRecorder::new(log));

    let outcome = agent
        .start(
            input("user_1", "look once"),
            ExecutionContext {
                cancellation: CancellationToken::new(),
                recorder: recorder.clone(),
                authorizer: Arc::new(AllowAllAuthorizer),
            },
        )
        .completion
        .await;

    assert!(matches!(outcome, ExecutionOutcome::Completed(_)));
    assert_eq!(recorder.deltas().len(), 2);
    assert!(recorder.pending_exchanges().is_empty());
}

#[tokio::test]
async fn ephemeral_recorder_settles_pending_exchange_during_tool_cancellation() {
    let log = OrderLog::new();
    let entered = Arc::new(Notify::new());
    let cleanup = Arc::new(Notify::new());
    let mut registry = ToolRegistry::new();
    registry
        .register(
            ScriptedTool::hanging("lookup", log)
                .with_entered_signal(entered.clone())
                .with_cleanup_signal(cleanup.clone()),
        )
        .expect("register hanging tool");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(false, true),
        CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&tool_call_message(
            "tool_turn",
            "call_1",
            "lookup",
        )))],
    ));
    let agent = AgentBuilder::new(model, prompt(), evaluator())
        .tools(registry.snapshot())
        .build()
        .expect("valid tool agent");
    let execution = agent.start_ephemeral(
        input("user_1", "start and cancel"),
        CancellationToken::new(),
        Arc::new(AllowAllAuthorizer),
    );

    entered.notified().await;
    execution.control.cancel();
    let outcome = execution.completion.await;
    cleanup.notified().await;

    assert_eq!(outcome, ExecutionOutcome::Cancelled);
}

#[tokio::test]
async fn dropping_event_consumer_does_not_change_completion() {
    let model = Arc::new(ScriptedModelService::completing(
        capabilities(false, false),
        CONTEXT_WINDOW_TOKENS,
        text_message("answer", "still completes"),
    ));
    let agent = AgentBuilder::new(model, prompt(), evaluator())
        .build()
        .expect("valid agent");
    let execution = agent.start_ephemeral(
        input("user_1", "hello"),
        CancellationToken::new(),
        Arc::new(AllowAllAuthorizer),
    );
    let agent_core::AgentExecution {
        events,
        completion,
        control: _,
    } = execution;
    drop(events);

    assert!(matches!(
        completion.await,
        ExecutionOutcome::Completed(message)
            if message == text_message("answer", "still completes")
    ));
}

#[tokio::test]
async fn connected_event_consumer_observes_reliable_terminal_event() {
    let model = Arc::new(ScriptedModelService::completing(
        capabilities(false, false),
        CONTEXT_WINDOW_TOKENS,
        text_message("answer", "done"),
    ));
    let agent = AgentBuilder::new(model, prompt(), evaluator())
        .build()
        .expect("valid agent");
    let execution = agent.start_ephemeral(
        input("user_1", "hello"),
        CancellationToken::new(),
        Arc::new(AllowAllAuthorizer),
    );
    let agent_core::AgentExecution {
        mut events,
        completion,
        control: _,
    } = execution;
    let event_task = tokio::spawn(async move {
        let mut terminal = None;
        while let Some(event) = events.next().await {
            if matches!(event, AgentEvent::ExecutionCompleted { .. }) {
                terminal = Some(event);
            }
        }
        terminal
    });

    assert!(matches!(completion.await, ExecutionOutcome::Completed(_)));
    assert!(event_task.await.expect("event task joins").is_some());
}
