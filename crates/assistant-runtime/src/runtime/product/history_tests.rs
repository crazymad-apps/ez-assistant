use super::*;
use crate::{StoredHistoryFacts, StoredMessageOrigin, StoredRunHistory};

fn window(partial: bool) -> StoredConversationWindow {
    let run = RunId::new("source-run").unwrap();
    StoredConversationWindow {
        generation: 1,
        start: 0,
        end: 2,
        total: 2,
        conversation: ConversationSnapshot::new(vec![
            ConversationMessage::User(agent_types::UserMessage {
                id: agent_types::MessageId::new("source-user").unwrap(),
                origin: Default::default(),
                transcript_visibility: Default::default(),
                parts: Vec::new(),
            }),
            ConversationMessage::Assistant(agent_types::AssistantMessage {
                id: agent_types::MessageId::new("source-assistant").unwrap(),
                purpose: Default::default(),
                model: agent_types::ModelIdentity::new(
                    agent_types::ProviderId::new("fixture").unwrap(),
                    "fixture",
                ),
                parts: Vec::new(),
                finish_reason: agent_types::FinishReason::Stop,
                usage: None,
            }),
        ]),
        history: Some(StoredHistoryFacts {
            runs: vec![StoredRunHistory {
                finished_after_message_id: None,
                run_id: run.clone(),
                input_id: None,
                user_message_id: Some(agent_types::MessageId::new("source-user").unwrap()),
                attempt: 2,
                agent_variant: assistant_protocol::AgentVariant::Build,
                approval_mode: assistant_protocol::ApprovalMode::Ask,
                reasoning_effort: None,
                agent_shell_kind: None,
                status: if partial {
                    None
                } else {
                    Some(assistant_protocol::RunStatus::Completed)
                },
                error: None,
                cancel_requested: false,
                created_at_ms: 10,
                started_at_ms: Some(20),
                finished_at_ms: if partial { None } else { Some(30) },
                inherited: true,
                partial,
                last_message_id: Some(agent_types::MessageId::new("source-assistant").unwrap()),
            }],
            message_origins: vec![StoredMessageOrigin {
                message_id: agent_types::MessageId::new("source-assistant").unwrap(),
                run_id: Some(run),
                input_id: None,
                attempt: Some(2),
                step: Some(3),
                inherited: true,
            }],
            ..Default::default()
        }),
    }
}

fn input_fact() -> crate::StoredInputHistory {
    crate::StoredInputHistory {
        input_id: assistant_protocol::InputId::new("source-input").unwrap(),
        user_message_id: agent_types::MessageId::new("source-user").unwrap(),
        queue_order: 7,
        agent_variant: assistant_protocol::AgentVariant::Build,
        origin: crate::InputOrigin::User,
        goal_binding: None,
        cross_session: None,
        channel_source: Some(crate::InputChannelSource::desktop_text()),
        agent_shell_target: None,
        accepted_at_ms: 10,
        inherited: false,
    }
}

#[test]
fn file_input_source_projects_device_and_proxy_labels_without_runtime_input_records() {
    let mut window = window(false);
    let mut input = input_fact();
    let device = assistant_protocol::DeviceId::new("source-device").unwrap();
    input.channel_source = Some(crate::InputChannelSource::Device(
        crate::DeviceInputSource {
            device_id: device.clone(),
            client_input_id: "device-request".into(),
            modality: crate::InputModality::SpeechTranscript,
            requested_output: crate::OutputPreference::Audio,
        },
    ));
    window.history.as_mut().unwrap().inputs.push(input.clone());
    let mut context = empty_child_projection();
    context.device_names.insert(device, "客厅设备".into());
    let items =
        project_conversation_with_history(&window.conversation, window.history.as_ref(), &context)
            .unwrap();
    let ConversationItem::User(user) = &items[0] else {
        panic!("user history");
    };
    assert!(
        matches!(&user.source, ConversationInputSourceSnapshot::Device { device_name, .. } if device_name == "客厅设备")
    );
    assert_eq!(user.input_id.as_ref().unwrap().as_str(), "source-input");

    input.origin = crate::InputOrigin::Runtime;
    input.channel_source = None;
    input.cross_session = Some(crate::CrossSessionInputEnvelope {
        binding: crate::CrossSessionInputBinding::ProxyReport {
            source_session_id: SessionId::new("actual-source").unwrap(),
            source_run_id: RunId::new("actual-run").unwrap(),
            source_goal_id: None,
            source_run_status: assistant_protocol::RunStatus::Failed,
        },
        reply_route: crate::ReplyRoute::SessionDefault,
    });
    window.history.as_mut().unwrap().inputs = vec![input];
    let items =
        project_conversation_with_history(&window.conversation, window.history.as_ref(), &context)
            .unwrap();
    let ConversationItem::User(user) = &items[0] else {
        panic!("user history");
    };
    assert!(
        matches!(&user.source, ConversationInputSourceSnapshot::ProxyReport { source_session_id, source_run_id, .. }
        if source_session_id.as_str() == "actual-source" && source_run_id.as_str() == "actual-run")
    );
}

#[test]
fn file_shell_intent_uses_frozen_actual_shell_without_executable_state() {
    for (actual, expected) in [
        (None, false),
        (Some(assistant_protocol::ShellKind::Bash), true),
    ] {
        let mut window = window(false);
        let mut input = input_fact();
        input.agent_shell_target = Some(assistant_protocol::ShellKind::Bash);
        let history = window.history.as_mut().unwrap();
        let run = &mut history.runs[0];
        run.input_id = Some(input.input_id.clone());
        run.agent_shell_kind = actual;
        history.message_origins.push(StoredMessageOrigin {
            message_id: input.user_message_id.clone(),
            run_id: Some(run.run_id.clone()),
            input_id: Some(input.input_id.clone()),
            attempt: Some(2),
            step: None,
            inherited: true,
        });
        history.inputs.push(input);
        let items = project_conversation_with_history(
            &window.conversation,
            window.history.as_ref(),
            &empty_child_projection(),
        )
        .unwrap();
        assert!(
            matches!(items[0], ConversationItem::ShellSwitchResult { success, .. } if success == expected)
        );
    }
}

#[test]
fn file_history_projects_without_an_executable_input_or_run_record() {
    let window = window(false);
    let items = project_conversation_with_history(
        &window.conversation,
        window.history.as_ref(),
        &empty_child_projection(),
    )
    .unwrap();
    let ConversationItem::User(user) = &items[0] else {
        panic!("user history");
    };
    assert!(user.input_id.is_none());
    let ConversationItem::Assistant(assistant) = &items[1] else {
        panic!("assistant history");
    };
    assert_eq!(
        assistant.run_id.as_ref().map(RunId::as_str),
        Some("source-run")
    );
    assert_eq!(assistant.attempt, Some(2));
    assert_eq!(assistant.step, Some(3));
    assert_eq!(assistant.run_created_at_ms, Some(10));
    assert_eq!(assistant.run_started_at_ms, Some(20));
    assert_eq!(
        assistant.status,
        Some(assistant_protocol::RunStatus::Completed)
    );
    assert_eq!(assistant.finished_at_ms, Some(30));
}

#[test]
fn partial_file_history_does_not_invent_a_terminal_status_or_finished_time() {
    let window = window(true);
    let items = project_conversation_with_history(
        &window.conversation,
        window.history.as_ref(),
        &empty_child_projection(),
    )
    .unwrap();
    let ConversationItem::Assistant(assistant) = &items[1] else {
        panic!("assistant history");
    };
    assert_eq!(assistant.step, Some(3));
    assert!(assistant.status.is_none());
    assert!(assistant.finished_at_ms.is_none());
}
