use super::*;
use crate::{
    ChannelOutput, ChannelOutputDispatchError, ChannelOutputDispatcher, ChannelOutputFuture,
    ChannelSpeechRequirementFuture, ChannelSpeechSegment, DeviceDeliveryPreference,
    DeviceInputSource, DevicePublicKey, InputChannelSource, InputModality, NewPairedDevice,
    OutputPreference, ResolvedChannelDelivery, SubmitSessionInputRequest,
};
use agent_types::{TranscriptVisibility, UserMessageOrigin};

#[derive(Default)]
struct CapturingDispatcher {
    outputs: Mutex<Vec<ChannelOutput>>,
    speeches: Mutex<Vec<ChannelSpeechSegment>>,
    failure: Option<ChannelOutputDispatchError>,
}

struct DeviceSourceGatedModel {
    capabilities: ModelCapabilities,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    calls: AtomicUsize,
}

impl ModelService for DeviceSourceGatedModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        8_192
    }

    fn stream(&self, _request: ModelRequest, _context: ModelCallContext) -> ModelStreamFuture<'_> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let entered = self.entered.clone();
        let release = self.release.clone();
        let message = match call {
            0 => assistant_text("controller-device-final", "controller accepted"),
            1 => assistant_text("target-device-final", "target completed"),
            _ => assistant_text("controller-report-device-final", "report returned"),
        };
        let events = message_events(&message);
        Box::pin(async move {
            if call == 0 {
                entered.notify_one();
                release.notified().await;
            }
            Ok(Box::pin(futures_util::stream::iter(events)) as agent_model::ModelEventStream)
        })
    }
}

impl CapturingDispatcher {
    fn failing() -> Self {
        Self {
            outputs: Mutex::new(Vec::new()),
            speeches: Mutex::new(Vec::new()),
            failure: Some(ChannelOutputDispatchError::Unavailable),
        }
    }

    fn cancelling() -> Self {
        Self {
            outputs: Mutex::new(Vec::new()),
            speeches: Mutex::new(Vec::new()),
            failure: Some(ChannelOutputDispatchError::Cancelled),
        }
    }

    async fn wait_for_output(&self) -> ChannelOutput {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(output) = self.outputs.lock().expect("output log").pop() {
                    return output;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("channel output")
    }
}

impl ChannelOutputDispatcher for CapturingDispatcher {
    fn dispatch(&self, output: ChannelOutput) -> ChannelOutputFuture {
        self.outputs.lock().expect("output log").push(output);
        let result = self.failure.map_or(Ok(()), Err);
        Box::pin(std::future::ready(result))
    }

    fn dispatch_speech(&self, segment: ChannelSpeechSegment) -> ChannelOutputFuture {
        self.speeches.lock().expect("speech log").push(segment);
        let result = self.failure.map_or(Ok(()), Err);
        Box::pin(std::future::ready(result))
    }

    fn requires_speech(
        &self,
        deliveries: Vec<ResolvedChannelDelivery>,
    ) -> ChannelSpeechRequirementFuture {
        Box::pin(std::future::ready(!deliveries.is_empty()))
    }
}

fn paired_device(device_id: &str, display_name: &str) -> NewPairedDevice {
    let mut public_key = [7; 32];
    public_key[0] = device_id
        .bytes()
        .fold(0_u8, |digest, byte| digest.wrapping_add(byte));
    NewPairedDevice {
        device_id: assistant_protocol::DeviceId::new(device_id).expect("device id"),
        display_name: display_name.to_owned(),
        public_key: DevicePublicKey::new(public_key),
        paired_at_ms: 1_000,
    }
}

fn device_session_input(
    session_id: assistant_protocol::SessionId,
    source: DeviceInputSource,
    message: &str,
) -> SubmitSessionInputRequest {
    SubmitSessionInputRequest {
        input: assistant_protocol::SubmitInputRequest {
            session_id,
            message: message.to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        },
        source: InputChannelSource::Device(source),
    }
}

fn speak_call(message_id: &str, call_id: &str, text: &str) -> AssistantMessage {
    AssistantMessage {
        id: MessageId::new(message_id).expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new(call_id).expect("tool call id"),
            name: ToolName::new("speak").expect("tool name"),
            arguments: json!({"text": text}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    }
}

#[tokio::test]
async fn controller_speak_dispatches_every_segment_in_tool_call_order() {
    let dispatcher = Arc::new(CapturingDispatcher::default());
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&speak_call(
                "speak-first-message",
                "speak-first-call",
                "第一版播报",
            ))),
            ModelScript::Events(message_events(&speak_call(
                "speak-last-message",
                "speak-last-call",
                "第二版播报",
            ))),
            ModelScript::Events(message_events(&speak_call(
                "speak-third-message",
                "speak-third-call",
                "最终简短播报。",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "speak-final-answer",
                "这是保留在 Desktop Conversation 中的完整回答。",
            ))),
        ],
    )))
    .with_channel_output_dispatcher(dispatcher.clone());
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let device = runtime
        .register_paired_device(paired_device("device-speak", "播报终端"))
        .await
        .expect("paired device");
    let accepted = runtime
        .submit_session_input(device_session_input(
            controller.session.session_id.clone(),
            DeviceInputSource {
                device_id: device.device_id.clone(),
                client_input_id: "client-speak".to_owned(),
                modality: InputModality::Text,
                requested_output: OutputPreference::Audio,
            },
            "请给我完整说明，并准备一段播报",
        ))
        .await
        .expect("device input");
    let terminal = wait_for_terminal(
        &runtime,
        &controller.session.session_id,
        &accepted.run.run_id,
    )
    .await;
    assert_eq!(
        terminal.status,
        assistant_protocol::RunStatus::Completed,
        "unexpected terminal error: {:?}",
        terminal.error
    );
    let output = dispatcher.wait_for_output().await;
    assert_eq!(
        output.assistant_text.as_deref(),
        Some("这是保留在 Desktop Conversation 中的完整回答。")
    );
    assert!(output.speech_completed);
    let speeches = dispatcher.speeches.lock().expect("speech log");
    assert_eq!(speeches.len(), 3);
    assert_eq!(speeches[0].text, "第一版播报");
    assert_eq!(speeches[1].text, "第二版播报");
    assert_eq!(speeches[2].text, "最终简短播报。");
}

#[tokio::test]
async fn interrupted_speak_is_a_successful_takeover_instead_of_a_tool_failure() {
    let dispatcher = Arc::new(CapturingDispatcher::cancelling());
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&speak_call(
                "interrupted-speak-message",
                "interrupted-speak-call",
                "这段播报被用户接管。",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "interrupted-speak-final",
                "用户接管后的正常结束。",
            ))),
        ],
    )))
    .with_channel_output_dispatcher(dispatcher.clone());
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let device = runtime
        .register_paired_device(paired_device("device-takeover", "接管测试终端"))
        .await
        .expect("paired device");
    let accepted = runtime
        .submit_session_input(device_session_input(
            controller.session.session_id.clone(),
            DeviceInputSource {
                device_id: device.device_id,
                client_input_id: "client-takeover".to_owned(),
                modality: InputModality::Text,
                requested_output: OutputPreference::Audio,
            },
            "测试用户接管",
        ))
        .await
        .expect("device input");

    let terminal = wait_for_terminal(
        &runtime,
        &controller.session.session_id,
        &accepted.run.run_id,
    )
    .await;
    assert_eq!(
        terminal.status,
        assistant_protocol::RunStatus::Completed,
        "unexpected terminal error: {:?}",
        terminal.error
    );
    assert_eq!(dispatcher.speeches.lock().expect("speech log").len(), 1);
}

#[tokio::test]
async fn twenty_first_speak_is_rejected_before_channel_dispatch() {
    let dispatcher = Arc::new(CapturingDispatcher::default());
    let mut scripts = (0..21)
        .map(|index| {
            ModelScript::Events(message_events(&speak_call(
                &format!("limited-speak-message-{index}"),
                &format!("limited-speak-call-{index}"),
                &format!("第{}段播报。", index + 1),
            )))
        })
        .collect::<Vec<_>>();
    scripts.push(ModelScript::Events(message_events(&assistant_text(
        "limited-speak-final-answer",
        "完整回答。",
    ))));
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        scripts,
    )))
    .with_channel_output_dispatcher(dispatcher.clone());
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let device = runtime
        .register_paired_device(paired_device("device-speak-limit", "播报上限终端"))
        .await
        .expect("paired device");
    let accepted = runtime
        .submit_session_input(device_session_input(
            controller.session.session_id.clone(),
            DeviceInputSource {
                device_id: device.device_id,
                client_input_id: "client-speak-limit".to_owned(),
                modality: InputModality::Text,
                requested_output: OutputPreference::Audio,
            },
            "验证播报分段上限",
        ))
        .await
        .expect("device input");

    assert_eq!(
        wait_for_terminal(
            &runtime,
            &controller.session.session_id,
            &accepted.run.run_id,
        )
        .await
        .status,
        assistant_protocol::RunStatus::Completed
    );
    let output = dispatcher.wait_for_output().await;
    assert!(output.speech_completed);
    let speeches = dispatcher.speeches.lock().expect("speech log");
    assert_eq!(speeches.len(), 20);
    assert_eq!(
        speeches.last().map(|segment| segment.text.as_str()),
        Some("第20段播报。")
    );
}

#[tokio::test]
async fn missing_speak_creates_one_hidden_delivery_reminder_in_the_same_output_cycle() {
    let dispatcher = Arc::new(CapturingDispatcher::default());
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text(
                "answer-before-reminder",
                "这是用户在 Desktop 中看到的完整回答。",
            ))),
            ModelScript::Events(message_events(&speak_call(
                "reminder-speak-message",
                "reminder-speak-call",
                "这是自然的简短播报。",
            ))),
            ModelScript::Events(message_events(&assistant_text("reminder-finished", ""))),
        ],
    ));
    let runtime = runtime(model.clone()).with_channel_output_dispatcher(dispatcher.clone());
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let device = runtime
        .register_paired_device(paired_device("device-reminder", "提醒测试终端"))
        .await
        .expect("paired device");
    let accepted = runtime
        .submit_session_input(device_session_input(
            controller.session.session_id.clone(),
            DeviceInputSource {
                device_id: device.device_id,
                client_input_id: "client-reminder".to_owned(),
                modality: InputModality::Text,
                requested_output: OutputPreference::TextAndAudio,
            },
            "请给我回答",
        ))
        .await
        .expect("device input");
    assert_eq!(
        wait_for_terminal(
            &runtime,
            &controller.session.session_id,
            &accepted.run.run_id,
        )
        .await
        .status,
        assistant_protocol::RunStatus::Completed
    );
    let output = dispatcher.wait_for_output().await;
    assert_eq!(
        output.assistant_text.as_deref(),
        Some("这是用户在 Desktop 中看到的完整回答。")
    );
    assert!(output.speech_completed);
    let speeches = dispatcher.speeches.lock().expect("speech log");
    assert_eq!(speeches.len(), 1);
    assert_eq!(speeches[0].text, "这是自然的简短播报。");
    drop(speeches);

    let requests = model.take_requests();
    assert_eq!(requests.len(), 3);
    let reminder = requests[1]
        .conversation
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::User(message)
                if message.origin == UserMessageOrigin::Runtime
                    && message.transcript_visibility == TranscriptVisibility::Hidden =>
            {
                Some(message)
            }
            _ => None,
        })
        .expect("hidden speech reminder");
    assert!(reminder.parts.iter().any(|part| {
        matches!(part, UserPart::InternalContext(part) if part.kind == "speech_delivery_reminder")
    }));
}

#[tokio::test]
async fn speech_delivery_reminder_is_not_repeated_when_the_model_still_omits_speak() {
    let dispatcher = Arc::new(CapturingDispatcher::default());
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text(
                "answer-without-speak",
                "完整回答",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "reminder-without-speak",
                "",
            ))),
        ],
    ));
    let runtime = runtime(model.clone()).with_channel_output_dispatcher(dispatcher.clone());
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let device = runtime
        .register_paired_device(paired_device("device-one-reminder", "单次提醒终端"))
        .await
        .expect("paired device");
    runtime
        .submit_session_input(device_session_input(
            controller.session.session_id,
            DeviceInputSource {
                device_id: device.device_id,
                client_input_id: "client-one-reminder".to_owned(),
                modality: InputModality::Text,
                requested_output: OutputPreference::Audio,
            },
            "请回答",
        ))
        .await
        .expect("device input");
    let output = dispatcher.wait_for_output().await;
    assert_eq!(output.assistant_text.as_deref(), Some("完整回答"));
    assert!(!output.speech_completed);
    assert!(dispatcher.speeches.lock().expect("speech log").is_empty());
    assert_eq!(model.take_requests().len(), 2);
}

#[tokio::test]
async fn failed_run_does_not_retract_an_already_dispatched_speech_segment() {
    let dispatcher = Arc::new(CapturingDispatcher::default());
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&speak_call(
                "speak-before-failure",
                "speak-before-failure-call",
                "这段内容不应播放。",
            ))),
            ModelScript::FailEstablishment(ModelError::Auth("fixture failure".to_owned())),
        ],
    )))
    .with_channel_output_dispatcher(dispatcher.clone());
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let device = runtime
        .register_paired_device(paired_device("device-failed-speak", "失败测试终端"))
        .await
        .expect("paired device");
    let accepted = runtime
        .submit_session_input(device_session_input(
            controller.session.session_id.clone(),
            DeviceInputSource {
                device_id: device.device_id,
                client_input_id: "client-failed-speak".to_owned(),
                modality: InputModality::Text,
                requested_output: OutputPreference::Audio,
            },
            "触发失败",
        ))
        .await
        .expect("device input");
    assert_eq!(
        wait_for_terminal(
            &runtime,
            &controller.session.session_id,
            &accepted.run.run_id,
        )
        .await
        .status,
        assistant_protocol::RunStatus::Failed
    );
    tokio::task::yield_now().await;
    assert!(dispatcher.outputs.lock().expect("output log").is_empty());
}

#[tokio::test]
async fn device_input_is_idempotent_and_keeps_its_frozen_output_preference() {
    let dispatcher = Arc::new(CapturingDispatcher::default());
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "device-answer",
            "device response",
        )))],
    ));
    let runtime = runtime_with_store(
        model.clone(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
    )
    .await
    .with_channel_output_dispatcher(dispatcher.clone());
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let device = runtime
        .register_paired_device(paired_device("device-living-room", "客厅终端"))
        .await
        .expect("paired device");
    let hosted_device = runtime
        .register_paired_device(paired_device("device-study", "书房终端"))
        .await
        .expect("hosted device");
    runtime
        .set_current_controller_output_hosting(
            assistant_protocol::SetCurrentControllerOutputHostingRequest {
                device_id: Some(hosted_device.device_id.clone()),
            },
        )
        .await
        .expect("set a different hosted device");
    let request = device_session_input(
        controller.session.session_id.clone(),
        DeviceInputSource {
            device_id: device.device_id.clone(),
            client_input_id: "client-input-1".to_owned(),
            modality: InputModality::SpeechTranscript,
            requested_output: OutputPreference::Audio,
        },
        "设备语音输入",
    );

    let first = runtime
        .submit_session_input(request.clone())
        .await
        .expect("first device input");
    let repeated = runtime
        .submit_session_input(request)
        .await
        .expect("idempotent device input");
    assert_eq!(repeated.input_id, first.input_id);
    assert_eq!(repeated.run.run_id, first.run.run_id);
    assert_eq!(
        wait_for_terminal(&runtime, &controller.session.session_id, &first.run.run_id,)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );

    let output = dispatcher.wait_for_output().await;
    assert_eq!(output.assistant_text.as_deref(), Some("device response"));
    assert_eq!(
        output.deliveries,
        vec![ResolvedChannelDelivery::Device {
            device_id: device.device_id.clone(),
            preference: DeviceDeliveryPreference::Frozen(OutputPreference::Audio),
        }]
    );
    let requests = model.take_requests();
    assert_eq!(requests.len(), 1);
    let ConversationMessage::User(user) = &requests[0].conversation.messages[0] else {
        panic!("device model request must start with its UserMessage")
    };
    let channel = user
        .parts
        .iter()
        .find_map(|part| match part {
            UserPart::InternalContext(part) if part.kind == "channel_input" => Some(part),
            _ => None,
        })
        .expect("device channel context");
    assert_eq!(
        channel.text,
        "<channel_input>\nsource: intelligent_terminal\ninput_modality: speech_transcript\nreply_preference: audio\nreply_instruction: Call the speak tool with a concise, natural spoken reply for this turn before final completion.\n</channel_input>"
    );

    runtime
        .revoke_paired_device(device.device_id.clone())
        .await
        .expect("revoke source device");
    let source = runtime
        .list_conversation_page(assistant_protocol::ListConversationPageRequest {
            owner: assistant_protocol::ConversationOwner::MainSession {
                session_id: controller.session.session_id,
            },
            cursor: None,
            limit: 20,
        })
        .await
        .expect("conversation after revoke")
        .snapshot
        .value
        .items
        .into_iter()
        .find_map(|item| match item {
            assistant_protocol::ConversationItem::User(user) => Some(user.source),
            _ => None,
        })
        .expect("device input source");
    assert_eq!(
        source,
        assistant_protocol::ConversationInputSourceSnapshot::Device {
            device_id: device.device_id,
            device_name: "已移除设备".to_owned(),
            modality: assistant_protocol::InputModalitySnapshot::SpeechTranscript,
            requested_output: assistant_protocol::OutputPreferenceSnapshot::Audio,
        }
    );
    super::super::recovery::recover_registries(
        store.load_runtime().await.expect("recover revoked history"),
    )
    .expect("revoked source remains recoverable");
}

#[tokio::test]
async fn trusted_device_input_can_target_a_standard_session() {
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "standard-device-answer",
            "standard session response",
        )))],
    )));
    let session = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("standard session");
    let device = runtime
        .register_paired_device(paired_device("device-standard", "普通会话终端"))
        .await
        .expect("paired device");
    let accepted = runtime
        .submit_session_input(device_session_input(
            session.session.session_id.clone(),
            DeviceInputSource {
                device_id: device.device_id,
                client_input_id: "client-standard".to_owned(),
                modality: InputModality::Text,
                requested_output: OutputPreference::Text,
            },
            "发送到普通会话",
        ))
        .await
        .expect("device input for a standard session");

    assert_eq!(
        wait_for_terminal(&runtime, &session.session.session_id, &accepted.run.run_id,)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let recovered = runtime
        .store
        .load_runtime()
        .await
        .expect("recover device input facts");
    super::super::recovery::recover_registries(recovered)
        .expect("standard-session Device input survives Runtime recovery");
}

#[tokio::test]
async fn hosting_rename_and_revoke_update_controller_without_duplicate_business_state() {
    let runtime = runtime(empty_model());
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let device = runtime
        .register_paired_device(paired_device("device-bedroom", "卧室终端"))
        .await
        .expect("paired device");

    let hosted = runtime
        .set_current_controller_output_hosting(
            assistant_protocol::SetCurrentControllerOutputHostingRequest {
                device_id: Some(device.device_id.clone()),
            },
        )
        .await
        .expect("set hosting");
    assert!(hosted.changed);
    assert_eq!(
        hosted
            .session
            .pc_output_hosting
            .as_ref()
            .map(|value| value.device_name.as_str()),
        Some("卧室终端")
    );
    assert!(
        !runtime
            .set_current_controller_output_hosting(
                assistant_protocol::SetCurrentControllerOutputHostingRequest {
                    device_id: Some(device.device_id.clone()),
                },
            )
            .await
            .expect("idempotent hosting")
            .changed
    );

    runtime
        .rename_paired_device(device.device_id.clone(), "书房终端".to_owned())
        .await
        .expect("rename device");
    assert_eq!(
        runtime
            .session(&controller.session.session_id)
            .expect("controller state")
            .summary()
            .expect("controller summary")
            .pc_output_hosting
            .as_ref()
            .map(|value| value.device_name.as_str()),
        Some("书房终端")
    );

    let revoked = runtime
        .revoke_paired_device(device.device_id.clone())
        .await
        .expect("revoke device");
    assert!(revoked.changed);
    assert_eq!(
        revoked.cleared_session_ids,
        vec![controller.session.session_id.clone()]
    );
    assert!(
        runtime
            .paired_device(&device.device_id)
            .expect("device registry")
            .is_none()
    );
    assert_eq!(
        runtime
            .registered_device(&device.device_id)
            .expect("durable device registry")
            .map(|registered| registered.lifecycle),
        Some(crate::DeviceLifecycle::Revoked)
    );
    assert!(
        runtime
            .session(&controller.session.session_id)
            .expect("controller state")
            .summary()
            .expect("controller summary")
            .pc_output_hosting
            .is_none()
    );
}

#[tokio::test]
async fn channel_dispatch_failure_does_not_rewrite_the_completed_conversation() {
    let dispatcher = Arc::new(CapturingDispatcher::failing());
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "desktop-answer",
            "durable answer",
        )))],
    )))
    .with_channel_output_dispatcher(dispatcher.clone());
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let device = runtime
        .register_paired_device(paired_device("device-failing-output", "输出终端"))
        .await
        .expect("paired device");
    runtime
        .set_current_controller_output_hosting(
            assistant_protocol::SetCurrentControllerOutputHostingRequest {
                device_id: Some(device.device_id),
            },
        )
        .await
        .expect("set hosting");

    let submitted = runtime
        .submit_input(SubmitInputRequest {
            session_id: controller.session.session_id.clone(),
            message: "desktop input".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("desktop input");
    assert_eq!(
        wait_for_terminal(
            &runtime,
            &controller.session.session_id,
            &submitted.run.run_id,
        )
        .await
        .status,
        assistant_protocol::RunStatus::Completed
    );
    let output = dispatcher.wait_for_output().await;
    assert_eq!(output.assistant_text.as_deref(), Some("durable answer"));
    assert_eq!(
        runtime
            .get_run(assistant_protocol::GetRunRequest {
                session_id: controller.session.session_id,
                run_id: submitted.run.run_id,
            })
            .await
            .expect("completed run")
            .run
            .status,
        assistant_protocol::RunStatus::Completed
    );
}

#[tokio::test]
async fn device_controller_delivery_report_returns_to_the_source_instead_of_pc_hosting() {
    let dispatcher = Arc::new(CapturingDispatcher::default());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let runtime = runtime(Arc::new(DeviceSourceGatedModel {
        capabilities: model_capabilities(true),
        entered: entered.clone(),
        release: release.clone(),
        calls: AtomicUsize::new(0),
    }))
    .with_channel_output_dispatcher(dispatcher.clone());
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let target = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("target");
    let source_device = runtime
        .register_paired_device(paired_device("device-source", "来源终端"))
        .await
        .expect("source device");
    let hosted_device = runtime
        .register_paired_device(paired_device("device-hosted", "托管终端"))
        .await
        .expect("hosted device");
    runtime
        .set_current_controller_output_hosting(
            assistant_protocol::SetCurrentControllerOutputHostingRequest {
                device_id: Some(hosted_device.device_id),
            },
        )
        .await
        .expect("set PC hosting");

    let source = runtime
        .submit_session_input(device_session_input(
            controller.session.session_id.clone(),
            DeviceInputSource {
                device_id: source_device.device_id.clone(),
                client_input_id: "device-proxy-source".to_owned(),
                modality: InputModality::Text,
                requested_output: OutputPreference::Text,
            },
            "send work to the target",
        ))
        .await
        .expect("device controller input");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("device source run started");

    let target_controller = runtime.session_for_test(&target.session.session_id);
    target_controller
        .lock_state()
        .expect("target state")
        .queue_paused_by_user = true;
    runtime
        .set_session_proxy(assistant_protocol::SetSessionProxyRequest {
            session_id: target.session.session_id.clone(),
            enabled: true,
        })
        .await
        .expect("enable target proxy");
    let receipt = runtime
        .controller_tool_coordinator()
        .deliver(
            &controller.session.session_id,
            &source.run.run_id,
            &assistant_protocol::ToolCallId::new("device-proxy-call").expect("call id"),
            &target.session.session_id,
            "delegated work".to_owned(),
            false,
        )
        .await
        .expect("controller delivery");
    release.notify_one();
    assert_eq!(
        wait_for_terminal(&runtime, &controller.session.session_id, &source.run.run_id,)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    target_controller
        .lock_state()
        .expect("target state")
        .queue_paused_by_user = false;
    runtime
        .wake_queue(target_controller)
        .expect("wake target queue");
    let target_run_id = assistant_protocol::RunId::new(receipt.run_id).expect("target run id");
    assert_eq!(
        wait_for_terminal(&runtime, &target.session.session_id, &target_run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );

    let controller_session = runtime.session_for_test(&controller.session.session_id);
    let report_run_id = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(run_id) = controller_session
                .lock_state()
                .expect("controller state")
                .inputs
                .values()
                .find_map(|input| {
                    let envelope = input.stored.cross_session.as_ref()?;
                    match (&envelope.binding, &envelope.reply_route) {
                        (
                            crate::CrossSessionInputBinding::ProxyReport { .. },
                            crate::ReplyRoute::Device { device_id, .. },
                        ) if device_id == &source_device.device_id => {
                            Some(input.latest_run_id.clone())
                        }
                        _ => None,
                    }
                })
            {
                break run_id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("source-routed proxy report");
    assert_eq!(
        wait_for_terminal(&runtime, &controller.session.session_id, &report_run_id,)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );

    for _ in 0..2 {
        assert_eq!(
            dispatcher.wait_for_output().await.deliveries,
            vec![ResolvedChannelDelivery::Device {
                device_id: source_device.device_id.clone(),
                preference: DeviceDeliveryPreference::Frozen(OutputPreference::Text),
            }]
        );
    }
}
