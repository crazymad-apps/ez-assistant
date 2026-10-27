//! 真实 Run 装配、resolve、授权、审批与执行路径；全部配置在内存，不访问日常 Home 或数据库。

mod conflicts;

use super::*;
use assistant_protocol::{
    AgentVariant, ApprovalDecision, DecideApprovalRequest, GetMcpConfigurationRequest,
    McpConfigurationMutation, McpServerKey, MutateMcpConfigurationRequest, RunStatus,
};

struct MemoryMcpSource(Mutex<(String, String)>);

impl MemoryMcpSource {
    fn fixture() -> Self {
        Self(Mutex::new(("r1".into(), json!({"mcpServers":{"fixture":{"command":"fixture-only","enabled":false,"env":{"CUSTOM":"old-private-value"}}}}).to_string())))
    }

    fn document(&self) -> String {
        self.0.lock().unwrap().1.clone()
    }
}

impl crate::McpConfigSource for MemoryMcpSource {
    fn load(&self) -> ConfigSourceFuture<'_> {
        Box::pin(async {
            let state = self.0.lock().unwrap();
            ConfigSourceLoad::Document(ConfigDocument::new(state.1.clone(), state.0.clone()))
        })
    }

    fn replace(&self, expected: Option<String>, contents: String) -> ConfigSourceReplaceFuture<'_> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            if expected.as_ref() != Some(&state.0) {
                return ConfigSourceReplace::Conflict(ConfigSourceLoad::Document(
                    ConfigDocument::new(state.1.clone(), state.0.clone()),
                ));
            }
            state.0.push('n');
            state.1 = contents.clone();
            ConfigSourceReplace::Applied(ConfigDocument::new(contents, state.0.clone()))
        })
    }
}

fn target() -> serde_json::Value {
    json!({"source":{"type":"user"}, "server_key":"fixture"})
}

fn patch() -> serde_json::Value {
    json!({"expected":{"target":target(), "expected_revision":"r1", "expected_definition_digest":""},"change":{"type":"patch","enabled":false,"environment":{"CUSTOM":{"mode":"replace","value":"new-private-value"}},"headers":{}}})
}

fn scripted(tool: &str, arguments: serde_json::Value) -> Arc<ScriptedModelService> {
    let mut call = assistant_tool_call("management-call", tool);
    let AssistantPart::ToolCall(part) = &mut call.parts[0] else {
        unreachable!()
    };
    part.arguments = arguments;
    Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&call)),
            ModelScript::Events(message_events(&assistant_text("finished", "done"))),
        ],
    ))
}

fn fixture(model: Arc<dyn ModelService>, source: Arc<MemoryMcpSource>) -> AssistantRuntime {
    runtime(model).with_mcp_services(
        source,
        Arc::new(crate::mcp::UnavailableMcpConnectionFactory),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    )
}

async fn submit(
    runtime: &AssistantRuntime,
    variant: AgentVariant,
    auto: bool,
) -> (SessionId, RunId) {
    let id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    if auto {
        set_auto_approval(runtime, &id).await;
    }
    let accepted = runtime
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant,
            session_id: id.clone(),
            message: "MCP management fixture".into(),
            attachment_ids: vec![],
            quotes: vec![],
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .unwrap();
    (id, accepted.run.run_id)
}

#[tokio::test]
async fn mcp_management_query_is_available_in_plan_without_connected_scope_and_masks_values() {
    let model = scripted("get_mcp_configuration", json!({}));
    let source = Arc::new(MemoryMcpSource::fixture());
    let runtime = fixture(model.clone(), source);
    let (id, run) = submit(&runtime, AgentVariant::Plan, false).await;
    assert_eq!(
        wait_for_terminal(&runtime, &id, &run).await.status,
        RunStatus::Completed
    );
    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    for name in ["get_mcp_configuration", "configure_mcp", "refresh_mcp"] {
        assert!(
            requests[0]
                .tools
                .iter()
                .any(|tool| tool.name.as_str() == name)
        );
    }
    let encoded = serde_json::to_string(&requests[1]).unwrap();
    assert!(!encoded.contains("old-private-value"));
    assert!(encoded.contains("r1"));
    assert!(encoded.contains("CUSTOM"));
    assert!(
        runtime
            .mcp_service
            .registry
            .catalog_snapshot()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn mcp_management_build_auto_saves_through_command_kernel_and_keeps_other_user_unchanged() {
    let model = scripted("configure_mcp", patch());
    let source = Arc::new(MemoryMcpSource::fixture());
    let other_source = Arc::new(MemoryMcpSource::fixture());
    let runtime = fixture(model.clone(), source.clone());
    let other = fixture(empty_model(), other_source.clone());
    let (id, run) = submit(&runtime, AgentVariant::Build, true).await;
    assert_eq!(
        wait_for_terminal(&runtime, &id, &run).await.status,
        RunStatus::Completed
    );
    assert!(source.document().contains("new-private-value"));
    assert!(!other_source.document().contains("new-private-value"));
    assert!(
        runtime
            .mcp_service
            .registry
            .catalog_snapshot()
            .unwrap()
            .is_empty()
    );
    let snapshot = runtime
        .get_mcp_configuration(GetMcpConfigurationRequest::default())
        .await
        .unwrap()
        .snapshot;
    assert_eq!(snapshot.revision, "r1n");
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("private-value")
    );
    let requests = model.take_requests();
    let results: Vec<_> = requests[1]
        .conversation
        .messages
        .iter()
        .filter(|message| matches!(message, ConversationMessage::Tool(_)))
        .collect();
    assert!(
        !serde_json::to_string(&results)
            .unwrap()
            .contains("private-value")
    );
    other
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn mcp_management_plan_denies_write_and_refresh_before_auto_or_ask() {
    for (tool, args) in [
        ("configure_mcp", patch()),
        (
            "refresh_mcp",
            json!({"target":target(), "expected_revision":"r1", "expected_definition_digest":""}),
        ),
    ] {
        let source = Arc::new(MemoryMcpSource::fixture());
        let original = source.document();
        let model = scripted(tool, args);
        let runtime = fixture(model.clone(), source.clone());
        let (id, run) = submit(&runtime, AgentVariant::Plan, true).await;
        assert_eq!(
            wait_for_terminal(&runtime, &id, &run).await.status,
            RunStatus::Completed
        );
        assert_eq!(source.document(), original);
        assert!(
            serde_json::to_string(&model.take_requests()[1])
                .unwrap()
                .contains("Plan cannot")
        );
    }
}

#[tokio::test]
async fn mcp_management_ask_rechecks_revision_after_approval_and_never_shows_new_secret() {
    for stale in [false, true] {
        let source = Arc::new(MemoryMcpSource::fixture());
        let runtime = fixture(scripted("configure_mcp", patch()), source.clone());
        let (id, run) = submit(&runtime, AgentVariant::Build, false).await;
        let approval = wait_for_pending_approval(&runtime, &id).await;
        assert!(
            !serde_json::to_string(&approval)
                .unwrap()
                .contains("private-value")
        );
        if stale {
            runtime
                .mutate_mcp_configuration(MutateMcpConfigurationRequest {
                    expected_revision: "r1".into(),
                    mutation: McpConfigurationMutation::SetEnabled {
                        server_key: McpServerKey::new("fixture").unwrap(),
                        enabled: true,
                    },
                })
                .await
                .unwrap();
        }
        runtime
            .decide_approval(DecideApprovalRequest {
                session_id: id.clone(),
                approval_id: approval.approval_id,
                decision: ApprovalDecision::AllowOnce,
            })
            .await
            .unwrap();
        assert_eq!(
            wait_for_terminal(&runtime, &id, &run).await.status,
            RunStatus::Completed
        );
        assert_eq!(source.document().contains("new-private-value"), !stale);
    }
}

#[tokio::test]
async fn mcp_management_rejects_startup_fields_before_approval() {
    let mut args = patch();
    args["change"]["command"] = json!("untrusted");
    let source = Arc::new(MemoryMcpSource::fixture());
    let original = source.document();
    let runtime = fixture(scripted("configure_mcp", args), source.clone());
    let (id, run) = submit(&runtime, AgentVariant::Build, false).await;
    assert_eq!(
        wait_for_terminal(&runtime, &id, &run).await.status,
        RunStatus::Completed
    );
    assert_eq!(source.document(), original);
}

#[tokio::test]
async fn mcp_management_cancelled_approval_never_writes_configuration() {
    let source = Arc::new(MemoryMcpSource::fixture());
    let original = source.document();
    let runtime = fixture(scripted("configure_mcp", patch()), source.clone());
    let (id, run) = submit(&runtime, AgentVariant::Build, false).await;
    wait_for_pending_approval(&runtime, &id).await;
    runtime
        .interrupt_run(InterruptRunRequest {
            session_id: id.clone(),
            run_id: run.clone(),
        })
        .await
        .unwrap();
    wait_for_terminal(&runtime, &id, &run).await;
    assert_eq!(source.document(), original);
}

#[tokio::test]
async fn mcp_management_refresh_disabled_target_never_connects_or_executes_business_tool() {
    let model = scripted(
        "refresh_mcp",
        json!({"target":target(),"expected_revision":"r1","expected_definition_digest":""}),
    );
    let source = Arc::new(MemoryMcpSource::fixture());
    let runtime = fixture(model.clone(), source);
    let (id, run) = submit(&runtime, AgentVariant::Build, true).await;
    assert_eq!(
        wait_for_terminal(&runtime, &id, &run).await.status,
        RunStatus::Completed
    );
    let requests = model.take_requests();
    let results: Vec<_> = requests[1]
        .conversation
        .messages
        .iter()
        .filter(|message| matches!(message, ConversationMessage::Tool(_)))
        .collect();
    let encoded = serde_json::to_string(&results).unwrap();
    assert!(encoded.contains("disabled"));
    assert!(!encoded.contains("unavailable"));
}

#[tokio::test]
async fn mcp_management_general_deny_rule_overrides_build_auto() {
    let source = Arc::new(MemoryMcpSource::fixture());
    let original = source.document();
    let model = scripted("configure_mcp", patch());
    let runtime = fixture(model.clone(), source.clone());
    let id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    set_auto_approval(&runtime, &id).await;
    runtime
        .replace_permission_document(assistant_protocol::ReplacePermissionDocumentRequest {
            scope: assistant_protocol::PermissionDocumentScope::Session {
                session_id: id.clone(),
            },
            expected_revision: assistant_protocol::PermissionDocumentRevision::Missing,
            document: assistant_protocol::PermissionDocumentDraft {
                schema_version: 1,
                rules: vec![assistant_protocol::PermissionRuleDefinition {
                    id: "deny-mcp-config".into(),
                    effect: assistant_protocol::PermissionRuleEffect::Deny,
                    variants: vec![AgentVariant::Build],
                    matcher: assistant_protocol::PermissionRuleMatcher::General(
                        assistant_protocol::PermissionGeneralMatcher {
                            tool_name: "configure_mcp".into(),
                        },
                    ),
                }],
            },
        })
        .await
        .unwrap();
    submit_completed_turn(&runtime, &id, "attempt managed configuration").await;
    assert_eq!(source.document(), original);
    assert!(
        serde_json::to_string(&model.take_requests()[1])
            .unwrap()
            .contains("denied by a permission rule")
    );
}
