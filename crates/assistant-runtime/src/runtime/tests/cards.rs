use super::*;
use crate::{
    CommittedPluginChange, PluginAudit, PluginFileError, PluginFilePort, PluginOperationKind,
    PluginOperationPhase, PluginOperationUpdate, PluginRecordChange, PluginSource,
    PreparedPluginPackage, StoredPlugin, StoredPluginOperation,
};
use assistant_protocol::*;
use futures_util::future::BoxFuture;

struct UiFiles;

impl PluginFilePort for UiFiles {
    fn read_ui_html(
        &self,
        _name: String,
        _sha: String,
        _path: String,
    ) -> BoxFuture<'_, Result<String, PluginFileError>> {
        Box::pin(async { Ok("<!doctype html><p>Generic UI</p>".into()) })
    }
    fn prepare(
        &self,
        _key: IdempotencyKey,
        _selection: PluginPackageSelection,
        _cancel: tokio_util::sync::CancellationToken,
        _progress: Arc<dyn Fn(u64) + Send + Sync>,
    ) -> BoxFuture<'_, Result<PreparedPluginPackage, PluginFileError>> {
        Box::pin(async { Err(PluginFileError::Unavailable) })
    }
    fn publish(
        &self,
        _key: IdempotencyKey,
        _selection: PluginPackageSelection,
    ) -> BoxFuture<'_, Result<(), PluginFileError>> {
        Box::pin(async { Err(PluginFileError::Unavailable) })
    }
    fn discard(
        &self,
        _key: IdempotencyKey,
        _selection: Option<PluginPackageSelection>,
    ) -> BoxFuture<'_, Result<(), PluginFileError>> {
        Box::pin(async { Ok(()) })
    }
    fn finish(
        &self,
        _key: IdempotencyKey,
        _change: CommittedPluginChange,
    ) -> BoxFuture<'_, Result<(), PluginFileError>> {
        Box::pin(async { Ok(()) })
    }
    fn audit(
        &self,
        _plugins: Vec<StoredPlugin>,
        _ops: Vec<StoredPluginOperation>,
    ) -> BoxFuture<'_, Result<PluginAudit, PluginFileError>> {
        Box::pin(async { Ok(PluginAudit::default()) })
    }
}

async fn install(runtime: &AssistantRuntime) {
    let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../packages/assistant-protocol/fixtures/plugin-ui.json"
    ))
    .unwrap();
    fixture["base"]["configuration"] = json!([
        {"name":"accent","label":"Accent","required":false,"secret":false}
    ]);
    let mut app = fixture["base"]["ui"]["entries"][0].clone();
    app["id"] = json!("counter-app");
    app["title"] = json!("Counter app");
    app["kind"] = json!("app");
    app.as_object_mut().unwrap().remove("snapshot_schema");
    app.as_object_mut()
        .unwrap()
        .remove("readable_snapshot_schemas");
    app["capabilities"] = json!(["post-message", "set-composer-draft"]);
    fixture["base"]["ui"]["entries"]
        .as_array_mut()
        .unwrap()
        .push(app);
    let mut op = StoredPluginOperation {
        key: IdempotencyKey::new("ui-fixture-install").unwrap(),
        kind: PluginOperationKind::Install,
        intent_json: "{}".into(),
        phase: PluginOperationPhase::Accepted,
        committed_json: "[]".into(),
        cancel_requested: false,
        error_code: None,
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    runtime
        .store
        .accept_plugin_operation(op.clone())
        .await
        .unwrap();
    for phase in [
        PluginOperationPhase::Preparing,
        PluginOperationPhase::Committing,
        PluginOperationPhase::Cleaning,
    ] {
        let expected_phase = op.phase;
        op.phase = phase;
        let plugin = if phase == PluginOperationPhase::Cleaning {
            PluginRecordChange::Upsert(Box::new(StoredPlugin {
                name: "ui-fixture".into(),
                source: PluginSource::Upload,
                active_version: Some("1.0.0".into()),
                active_sha256: Some("a".repeat(64)),
                active_manifest_json: Some(fixture["base"].to_string()),
                enabled: true,
                parameters_json: "{}".into(),
                software_paths_json: "{}".into(),
                market_status: None,
                created_at_ms: 1,
                updated_at_ms: 1,
            }))
        } else {
            PluginRecordChange::Keep
        };
        runtime
            .store
            .advance_plugin_operation(PluginOperationUpdate {
                expected_phase,
                next: op.clone(),
                plugin,
            })
            .await
            .unwrap();
    }
}

fn publish_message() -> AssistantMessage {
    let mut message = assistant_tool_call("card-call-message", "publish_card");
    if let AssistantPart::ToolCall(call) = &mut message.parts[0] {
        call.arguments = json!({"plugin":"ui-fixture","entry":"counter","arguments":{"count":1}});
    }
    message
}

#[tokio::test]
async fn child_cards_keep_the_real_child_owner() {
    let mut delegate = assistant_tool_call("delegate-card", "delegate_task");
    if let AssistantPart::ToolCall(call) = &mut delegate.parts[0] {
        call.arguments = json!({"title":"Show card","task":"Publish the declared counter card"});
    }
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&delegate)),
            ModelScript::Events(message_events(&publish_message())),
            ModelScript::Events(message_events(&assistant_text(
                "child-card-done",
                "child done",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "parent-card-done",
                "parent done",
            ))),
        ],
    ));
    let runtime = runtime(model).with_plugin_files(Arc::new(UiFiles));
    install(&runtime).await;
    runtime.config_registry.replace_document_for_test(&format!("{TEST_CONFIG}\n[agent.defaults.delegation]\nmax_tasks_per_run=1\nmax_concurrent_tasks=1\ntask_timeout_ms=2000\nmax_steps=16\nmax_tool_calls=16\nmax_output_tokens=4096\n"));
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    set_auto_approval(&runtime, &session).await;
    let run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            session_id: session.clone(),
            message: "delegate".into(),
            variant: AgentVariant::Build,
            mode: SubmitInputMode::Normal,
            attachment_ids: vec![],
            quotes: vec![],
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .unwrap()
        .run;
    assert_eq!(
        wait_for_terminal(&runtime, &session, &run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    let history = runtime
        .store
        .load_conversation_history(ConversationOwner::MainSession {
            session_id: session.clone(),
        })
        .await
        .unwrap();
    assert_eq!(history.facts.child_tasks.len(), 1);
    let owner = ConversationOwner::ChildTask {
        session_id: session.clone(),
        child_task_id: history.facts.child_tasks[0].child_task_id.clone(),
    };
    let child = runtime
        .list_message_cards(ListMessageCardsRequest {
            owner: owner.clone(),
            run_ids: vec![],
            message_ids: vec![],
            after: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(child.cards.len(), 1);
    assert_eq!(child.cards[0].owner, owner);
    let parent = runtime
        .list_message_cards(ListMessageCardsRequest {
            owner: ConversationOwner::MainSession {
                session_id: session,
            },
            run_ids: vec![run.run_id],
            message_ids: vec![],
            after: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert!(parent.cards.is_empty());
}

#[derive(Default)]
struct HoldUiTool {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

struct UiMcp(
    Arc<super::product::WorkingMcpTestConnection>,
    bool,
    Option<Arc<HoldUiTool>>,
);

impl crate::McpConnection for UiMcp {
    fn list_tools_page(
        &self,
        cursor: Option<String>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> crate::McpConnectionFuture<'_, crate::McpToolPage> {
        Box::pin(async move {
            let mut page = self.0.list_tools_page(cursor, cancel).await?;
            for tool in &mut page.tools {
                tool.meta = Some(json!({"ui":{"resourceUri":"ui://test/card"}}));
            }
            Ok(page)
        })
    }
    fn call_tool_once(
        &self,
        name: String,
        args: serde_json::Map<String, serde_json::Value>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> crate::McpConnectionFuture<'_, crate::McpRawCallResult> {
        Box::pin(async move {
            if let Some(hold) = &self.2 {
                hold.entered.notify_one();
                // 模拟不及时响应取消的外部服务，严格换域必须报告本地收尾尚未完成。
                hold.release.notified().await;
            }
            let mut result = self.0.call_tool_once(name, args, cancel).await?;
            result.meta = Some(
                json!({"private": if self.1 { "MCP_UI_PRIVATE_RESULT".repeat(20_000) } else { "MCP_UI_PRIVATE_RESULT".into() }}),
            );
            Ok(result)
        })
    }
    fn read_ui_resource(
        &self,
        uri: String,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> crate::McpConnectionFuture<'_, McpUiResource> {
        Box::pin(async move {
            Ok(McpUiResource {
                uri,
                html: "<p>Generic UI</p>".into(),
                ui: None,
            })
        })
    }
    fn close(
        &self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> crate::McpConnectionFuture<'_, ()> {
        self.0.close(cancel)
    }
}

struct UiMcpFactory(Arc<UiMcp>);
impl crate::McpConnectionFactory for UiMcpFactory {
    fn connect(
        &self,
        _server: crate::McpServerConfig,
        _options: crate::McpConnectionOptions,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> crate::McpConnectionFuture<'_, Arc<dyn crate::McpConnection>> {
        Box::pin(async move { Ok(self.0.clone() as Arc<dyn crate::McpConnection>) })
    }
}

#[tokio::test]
async fn mcp_results_publish_once_without_disclosing_private_ui_data_to_model() {
    mcp_card_case(false).await;
}

#[tokio::test]
async fn oversized_ui_copy_keeps_business_result_and_does_not_retry_the_tool() {
    mcp_card_case(true).await;
}

async fn mcp_card_case(oversized: bool) {
    let mut call = assistant_tool_call("mcp-ui-call", "call_mcp_tool");
    if let AssistantPart::ToolCall(call) = &mut call.parts[0] {
        call.arguments = json!({"server":"github","tool":"list_issues","arguments":{}});
    }
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&call)),
            ModelScript::Events(message_events(&assistant_text("mcp-card-done", "done"))),
        ],
    ));
    let connection = Arc::new(UiMcp(
        Arc::new(super::product::WorkingMcpTestConnection {
            calls: AtomicUsize::new(0),
        }),
        oversized,
        None,
    ));
    let runtime = runtime(model.clone()).with_mcp_services(
        Arc::new(super::product::StaticMcpTestSource),
        Arc::new(UiMcpFactory(connection.clone())),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime.bootstrap_mcp().await.unwrap();
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            session_id: session.clone(),
            message: "MCP card".into(),
            variant: AgentVariant::Build,
            mode: SubmitInputMode::Normal,
            attachment_ids: vec![],
            quotes: vec![],
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .unwrap()
        .run;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let approvals = runtime
                .list_pending_approvals(ListPendingApprovalsRequest {
                    session_id: session.clone(),
                })
                .await
                .unwrap()
                .approvals;
            for approval in approvals {
                runtime
                    .decide_approval(DecideApprovalRequest {
                        session_id: session.clone(),
                        approval_id: approval.approval_id,
                        decision: ApprovalDecision::AllowOnce,
                    })
                    .await
                    .unwrap();
            }
            if runtime
                .get_run(GetRunRequest {
                    session_id: session.clone(),
                    run_id: run.run_id.clone(),
                })
                .await
                .unwrap()
                .run
                .status
                == RunStatus::Completed
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let owner = ConversationOwner::MainSession {
        session_id: session,
    };
    let listed = runtime
        .list_message_cards(ListMessageCardsRequest {
            owner: owner.clone(),
            run_ids: vec![run.run_id],
            message_ids: vec![],
            after: None,
            limit: 10,
        })
        .await
        .unwrap();
    let requests = serde_json::to_string(&model.take_requests()).unwrap();
    assert!(!requests.contains("MCP_UI_PRIVATE_RESULT"));
    if oversized {
        assert!(listed.cards.is_empty());
        assert!(requests.contains("业务结果已返回，卡片未保存"));
        assert_eq!(connection.0.calls.load(Ordering::SeqCst), 1);
        return;
    }
    assert_eq!(listed.cards.len(), 1);
    let page = runtime
        .mount_message_card(MountMessageCardRequest {
            owner,
            card_id: listed.cards[0].card_id.clone(),
            client_instance_id: "mcp-client".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        page.tool_result["_meta"]["private"],
        "MCP_UI_PRIVATE_RESULT"
    );

    assert_eq!(
        connection.0.calls.load(Ordering::SeqCst),
        1,
        "mount must not rerun MCP business call"
    );
}

#[tokio::test]
async fn cards_publish_without_client_and_snapshot_cas_rejects_old_instances() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&publish_message())),
            ModelScript::Events(message_events(&assistant_text("card-done", "done"))),
        ],
    ));
    let runtime = runtime(model.clone()).with_plugin_files(Arc::new(UiFiles));
    install(&runtime).await;
    let plugins = runtime.list_plugins().await.unwrap();
    assert_eq!(
        plugins.plugins[0].ui.as_ref().unwrap().entries[0].id,
        "counter"
    );
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let input = SubmitInputRequest {
        delivery: Default::default(),
        session_id: session.clone(),
        message: "publish".into(),
        variant: AgentVariant::Build,
        mode: SubmitInputMode::Normal,
        attachment_ids: vec![],
        quotes: vec![],
        skill_name: None,
        mcp_server_key: None,
        idempotency_key: None,
    };
    let run = runtime.submit_started_input(input).await.unwrap().run;
    assert_eq!(
        wait_for_terminal(&runtime, &session, &run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    let owner = ConversationOwner::MainSession {
        session_id: session.clone(),
    };
    let list = runtime
        .list_message_cards(ListMessageCardsRequest {
            owner: owner.clone(),
            run_ids: vec![run.run_id.clone()],
            message_ids: vec![],
            after: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(
        list.cards.len(),
        1,
        "tool must publish without a mounted client"
    );
    let card = &list.cards[0];
    let mount = |client: &str| MountMessageCardRequest {
        owner: owner.clone(),
        card_id: card.card_id.clone(),
        client_instance_id: client.into(),
    };
    let first = runtime.mount_message_card(mount("client-a")).await.unwrap();
    let second = runtime.mount_message_card(mount("client-b")).await.unwrap();
    let write = |instance: &str, revision| SaveMessageCardSnapshotRequest {
        owner: owner.clone(),
        card_id: card.card_id.clone(),
        instance_id: instance.into(),
        expected_revision: revision,
        snapshot: MessageCardSnapshot {
            schema: "v1".into(),
            value: json!({"private":"PRIVATE_UI_SNAPSHOT"}),
        },
    };
    assert!(
        runtime
            .save_message_card_snapshot(write(&first.instance_id, 0))
            .await
            .unwrap()
            .saved
    );
    let conflict = runtime
        .save_message_card_snapshot(write(&second.instance_id, 0))
        .await
        .unwrap();
    assert!(!conflict.saved);
    assert_eq!(conflict.revision, 1);
    let third = runtime.mount_message_card(mount("client-a")).await.unwrap();
    assert_eq!(
        third.snapshot.unwrap().value["private"],
        "PRIVATE_UI_SNAPSHOT"
    );
    assert!(
        runtime
            .save_message_card_snapshot(write(&first.instance_id, 1))
            .await
            .is_err()
    );
    runtime
        .release_message_card(ReleaseMessageCardRequest {
            owner: owner.clone(),
            card_id: card.card_id.clone(),
            instance_id: third.instance_id.clone(),
        })
        .await
        .unwrap();
    assert!(
        runtime
            .save_message_card_snapshot(write(&third.instance_id, 1))
            .await
            .is_err()
    );
    let other = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let mut wrong = mount("client-other");
    wrong.owner = ConversationOwner::MainSession { session_id: other };
    assert!(runtime.mount_message_card(wrong).await.is_err());
    let forked = runtime
        .fork_session(ForkSessionRequest {
            session_id: session.clone(),
            fork_point: assistant_protocol::MessageId::new("card-done").unwrap(),
            expected_generation: 1,
        })
        .await
        .unwrap();
    let copied = runtime
        .list_message_cards(ListMessageCardsRequest {
            owner: ConversationOwner::MainSession {
                session_id: forked.session.session_id,
            },
            run_ids: vec![run.run_id.clone()],
            message_ids: vec![assistant_protocol::MessageId::new("card-call-message").unwrap()],
            after: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(copied.cards.len(), 1);
    assert_ne!(copied.cards[0].card_id, card.card_id);
    assert!(copied.cards[0].inherited);
    let before = runtime.store.list_plugins().await.unwrap().remove(0);
    let mut next = before.clone();
    next.parameters_json = json!({"accent":"updated"}).to_string();
    next.updated_at_ms += 1;
    runtime
        .store
        .change_plugin_settings(crate::PluginSettingsChange {
            expected: before,
            next,
        })
        .await
        .unwrap();
    assert!(
        runtime
            .save_message_card_snapshot(write(&second.instance_id, 1))
            .await
            .is_err(),
        "changed configuration retires an old page even before its client observes the event"
    );
    let current = runtime.mount_message_card(mount("client-b")).await.unwrap();
    assert!(
        runtime
            .save_message_card_snapshot(write(&current.instance_id, 1))
            .await
            .unwrap()
            .saved
    );
    let before = runtime.store.list_plugins().await.unwrap().remove(0);
    let mut next = before.clone();
    next.enabled = false;
    next.updated_at_ms += 1;
    runtime
        .store
        .change_plugin_settings(crate::PluginSettingsChange {
            expected: before,
            next,
        })
        .await
        .unwrap();
    assert!(
        runtime
            .save_message_card_snapshot(write(&current.instance_id, 1))
            .await
            .is_err()
    );
    assert!(
        !serde_json::to_string(&runtime.conversation_snapshot(&session).await.unwrap())
            .unwrap()
            .contains("PRIVATE_UI_SNAPSHOT")
    );
    assert!(!format!("{list:?}").contains("PRIVATE_UI_SNAPSHOT"));
    assert_eq!(
        model.take_requests().len(),
        2,
        "mount/save must not invoke a model"
    );
}

#[tokio::test]
async fn declared_apps_open_without_creating_a_run_or_revealing_card_data() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [],
    ));
    let runtime = runtime(model.clone()).with_plugin_files(Arc::new(UiFiles));
    install(&runtime).await;
    let listed = runtime.list_ui_apps().await.unwrap();
    assert_eq!(listed.apps.len(), 1);
    assert_eq!(listed.apps[0].entry.id, "counter-app");
    let read = runtime
        .read_ui_app(ReadUiAppRequest {
            target: listed.apps[0].target.clone(),
            arguments: std::collections::BTreeMap::from([("count".into(), json!(2))]),
            mount: None,
        })
        .await
        .unwrap();
    assert!(read.resource.html.contains("Generic UI"));
    assert!(!format!("{read:?}").contains("Generic UI"));
    assert!(
        runtime
            .read_ui_app(ReadUiAppRequest {
                target: listed.apps[0].target.clone(),
                arguments: std::collections::BTreeMap::from([("count".into(), json!("wrong"))]),
                mount: None,
            })
            .await
            .is_err()
    );
    assert!(
        runtime
            .read_ui_app(ReadUiAppRequest {
                target: UiAppTarget::Plugin {
                    name: "ui-fixture".into(),
                    entry_id: "counter".into()
                },
                arguments: Default::default(),
                mount: None,
            })
            .await
            .is_err()
    );
    assert!(model.take_requests().is_empty());
}

async fn draft_tool_runtime(hold: Option<Arc<HoldUiTool>>) -> (Arc<AssistantRuntime>, Arc<UiMcp>) {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [],
    ));
    let connection = Arc::new(UiMcp(
        Arc::new(super::product::WorkingMcpTestConnection {
            calls: AtomicUsize::new(0),
        }),
        false,
        hold,
    ));
    let mut runtime = runtime(model).with_mcp_services(
        Arc::new(super::product::StaticMcpTestSource),
        Arc::new(UiMcpFactory(connection.clone())),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime.config.shutdown_timeout = Duration::from_millis(80);
    runtime.bootstrap_mcp().await.unwrap();
    let key = McpServerKey::new("github").unwrap();
    let projection = runtime
        .mcp_service
        .registry
        .projections()
        .unwrap()
        .remove(&key)
        .unwrap();
    let fingerprint = projection
        .fingerprint
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let source = runtime
        .mcp_ui_tool_source(
            &McpServerTarget {
                source: projection.source,
                server_key: key,
            },
            &fingerprint,
        )
        .unwrap();
    assert!(
        runtime
            .mount_ui_tools(
                "page".into(),
                source,
                UiAppScope::Draft,
                "Fixture app".into()
            )
            .unwrap()
    );
    (Arc::new(runtime), connection)
}

async fn request_fixture_tool(
    runtime: &Arc<AssistantRuntime>,
    action: UiToolAction,
) -> UiToolState {
    runtime
        .ui_tool(UiToolRequest {
            instance_id: "page".into(),
            action,
        })
        .await
        .unwrap()
}

async fn wait_for_app_approval(runtime: &Arc<AssistantRuntime>) -> UiToolApproval {
    assert!(matches!(
        request_fixture_tool(
            runtime,
            UiToolAction::Start {
                request_id: "call".into(),
                name: "list_issues".into(),
                arguments: Default::default(),
                user_gesture: None,
            }
        )
        .await,
        UiToolState::Pending
    ));
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let UiToolState::Approval { approval } = request_fixture_tool(
                runtime,
                UiToolAction::Poll {
                    request_id: "call".into(),
                },
            )
            .await
            {
                return approval;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("page approval")
}

#[tokio::test]
async fn confirmed_shutdown_covers_app_only_work_and_cancels_pending_approval() {
    let (runtime, connection) = draft_tool_runtime(None).await;
    let empty = runtime.preview_shutdown().await.unwrap();
    assert_eq!(empty.counts(), (0, 0, 0));
    let approval = wait_for_app_approval(&runtime).await;
    assert_eq!(approval.scope, UiAppScope::Draft);
    assert_eq!(approval.app_title, "Fixture app");
    assert!(!runtime.begin_confirmed_shutdown(&empty).await.unwrap());
    let current = runtime.preview_shutdown().await.unwrap();
    assert_eq!(current.counts(), (1, 1, 0));
    assert!(runtime.begin_confirmed_shutdown(&current).await.unwrap());
    runtime.finish_confirmed_shutdown().await.unwrap();
    assert!(
        runtime
            .approval_registry
            .app_approval(&approval.approval_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(connection.0.calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn confirmed_shutdown_does_not_accept_incomplete_app_tool_cleanup() {
    let hold = Arc::new(HoldUiTool::default());
    let (runtime, _) = draft_tool_runtime(Some(hold.clone())).await;
    let approval = wait_for_app_approval(&runtime).await;
    request_fixture_tool(
        &runtime,
        UiToolAction::Decide {
            request_id: "call".into(),
            approval_id: approval.approval_id,
            decision: ApprovalDecision::AllowOnce,
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), hold.entered.notified())
        .await
        .expect("remote call entered");
    let work = runtime.preview_shutdown().await.unwrap();
    assert_eq!(work.counts(), (1, 0, 0));
    assert!(runtime.begin_confirmed_shutdown(&work).await.unwrap());
    assert!(runtime.finish_confirmed_shutdown().await.is_err());
    assert_eq!(runtime.lifecycle().unwrap(), RuntimeLifecycle::ShuttingDown);
    hold.release.notify_one();
    runtime.finish_confirmed_shutdown().await.unwrap();
}
