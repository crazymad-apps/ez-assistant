//! 真 HTTP 身份边界与临时 SQLite 用户域；确定性模型仅从测试装配注入。
pub(super) mod models;
mod resources;
mod tools;
use super::*;
use agent_model::{
    ModelCallContext, ModelError, ModelRequest, ModelService, ModelServiceBundle, ModelStreamFuture,
};
use assistant_protocol::*;
use assistant_runtime::{
    AssistantRuntime, ModelServiceFactory, ModelServiceFactoryError, ModelServiceFactoryRequest,
};
use std::time::Duration;

impl Fixture {
    async fn ready(&self, token: &str) -> reqwest::Response {
        self.request(reqwest::Method::POST, "/runtime/ensure-ready", Some(token))
            .send()
            .await
            .unwrap()
    }
    async fn services(&self, token: &str) -> Arc<crate::http::ReadyServices> {
        self.state
            .domains
            .as_ref()
            .unwrap()
            .ensure(&self.permit(token))
            .await
            .unwrap()
    }
    async fn command(&self, token: &str, command: RuntimeCommand) -> reqwest::Response {
        self.request(reqwest::Method::POST, "/commands", Some(token))
            .json(&json!({"request_id":"test", "command":{"scope":"runtime", "payload":command}}))
            .send()
            .await
            .unwrap()
    }
    async fn stop(&mut self) {
        self.shutdown.cancel();
        if let Some(task) = self.domain_task.take() {
            tokio::time::timeout(Duration::from_secs(14), task)
                .await
                .unwrap()
                .unwrap();
        }
    }
}

#[tokio::test]
async fn concurrent_readiness_routes_one_runtime_per_user_and_restores_after_logout() {
    let mut f = Fixture::with_domains(true, None).await;
    let alice = f.login("alice").await;
    let bob = f.login("bob").await;
    assert!(
        !f.home
            .path()
            .join(format!("users/{CENTER_ID}_1/data/runtime.sqlite3"))
            .exists()
    );
    let (a, b, a2) = tokio::join!(f.ready(&alice), f.ready(&bob), f.ready(&alice));
    assert_eq!(
        (a.status(), b.status(), a2.status()),
        (
            StatusCode::NO_CONTENT,
            StatusCode::NO_CONTENT,
            StatusCode::NO_CONTENT
        )
    );
    let a = f.services(&alice).await;
    let b = f.services(&bob).await;
    assert!(Arc::ptr_eq(&a, &f.services(&alice).await));
    assert!(!Arc::ptr_eq(&a, &b));
    let created = f
        .command(
            &alice,
            RuntimeCommand::CreateSession(CreateSessionRequest {
                title: Some("Alice private".into()),
                ..Default::default()
            }),
        )
        .await;
    assert_eq!(created.status(), StatusCode::OK);
    let created = created.json::<Value>().await.unwrap();
    let id: SessionId = serde_json::from_value(
        created["result"]["payload"]["payload"]["session"]["session_id"].clone(),
    )
    .unwrap();
    assert_eq!(
        f.command(
            &bob,
            RuntimeCommand::GetSessionView(GetSessionViewRequest {
                session_id: id.clone()
            })
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let again = f.login("alice").await;
    assert!(Arc::ptr_eq(&a, &f.services(&again).await));
    let logout = f
        .request(reqwest::Method::POST, "/auth/logout", Some(&alice))
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::OK);
    assert_eq!(a.runtime.lifecycle().unwrap(), RuntimeLifecycle::Stopped);
    assert_eq!(b.runtime.lifecycle().unwrap(), RuntimeLifecycle::Running);
    assert!(f.state.access.credentials.authenticate(&again).is_none());
    let latest = f.login("alice").await;
    assert_eq!(f.ready(&latest).await.status(), StatusCode::NO_CONTENT);
    assert!(!Arc::ptr_eq(&a, &f.services(&latest).await));
    assert_eq!(
        f.command(
            &latest,
            RuntimeCommand::GetSessionView(GetSessionViewRequest { session_id: id })
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert!(!f.home.path().join("users/_personal/data").exists());
    assert_eq!(
        f.ready("bootstrap-secret").await.status(),
        StatusCode::FORBIDDEN
    );
    f.stop().await;
}

#[tokio::test]
async fn initialization_failure_keeps_login_and_is_retryable_without_affecting_other_user() {
    let mut f = Fixture::with_domains(true, None).await;
    let alice = f.login("alice").await;
    let bob = f.login("bob").await;
    let home = f.home.path().join(format!("users/{CENTER_ID}_1"));
    // 在第一次开库前使用户根不可用；修复夹具后同一凭据可重试。
    std::fs::rename(&home, home.with_extension("saved")).unwrap();
    std::fs::write(&home, "not a directory").unwrap();
    assert_eq!(
        f.ready(&alice).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(f.session(&alice).await.status(), StatusCode::OK);
    assert_eq!(f.ready(&bob).await.status(), StatusCode::NO_CONTENT);
    std::fs::remove_file(&home).unwrap();
    std::fs::rename(home.with_extension("saved"), &home).unwrap();
    assert_eq!(f.ready(&alice).await.status(), StatusCode::NO_CONTENT);
    f.stop().await;
}

#[tokio::test]
async fn center_models_are_unavailable_and_local_model_mutations_are_rejected() {
    let mut f = Fixture::with_domains(true, None).await;
    let alice = f.login("alice").await;
    let services = f.services(&alice).await;
    assert!(
        services
            .runtime
            .set_default_model(SetDefaultModelRequest { selection: None })
            .await
            .is_err()
    );
    let id = services
        .runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let response = services
        .runtime
        .get_session_view(GetSessionViewRequest { session_id: id })
        .await
        .unwrap();
    let capabilities = response.snapshot.value.composer_capabilities;
    assert!(capabilities.selected_model.is_none());
    assert_eq!(
        capabilities.model_error.unwrap().code,
        RuntimeErrorCode::InvalidRequest
    );
    f.stop().await;
}

/// 建立请求后等待测试放行，取消由正式 Runtime 的取消树传入。
struct HeldModel {
    started: tokio::sync::Semaphore,
    release: CancellationToken,
    inner: agent_testkit::ScriptedModelService,
}
impl HeldModel {
    fn new() -> Arc<Self> {
        let message = agent_types::AssistantMessage {
            id: agent_types::MessageId::new("answer").unwrap(),
            model: agent_types::ModelIdentity::new(
                agent_types::ProviderId::new("local").unwrap(),
                "fixture",
            ),
            parts: vec![agent_types::AssistantPart::Text(agent_types::TextPart {
                id: agent_types::PartId::new("text").unwrap(),
                text: "done".into(),
            })],
            finish_reason: agent_types::FinishReason::Stop,
            usage: None,
        };
        Arc::new(Self {
            started: tokio::sync::Semaphore::new(0),
            release: CancellationToken::new(),
            inner: agent_testkit::ScriptedModelService::new(
                agent_model::ModelCapabilities {
                    streaming: true,
                    tool_calls: true,
                    tool_choice: agent_model::ToolChoiceCapabilities {
                        auto: true,
                        none: true,
                        required: true,
                        named: true,
                    },
                    ..Default::default()
                },
                32768,
                (0..8).map(|_| {
                    agent_testkit::ModelScript::Events(agent_testkit::message_events(&message))
                }),
            ),
        })
    }
    async fn started(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.started.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
    }
}
impl ModelService for HeldModel {
    fn capabilities(&self) -> &agent_model::ModelCapabilities {
        self.inner.capabilities()
    }
    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }
    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        Box::pin(async move {
            self.started.add_permits(1);
            tokio::select! { () = context.cancellation.cancelled() => return Err(ModelError::Cancelled), () = self.release.cancelled() => {} }
            self.inner.stream(request, context).await
        })
    }
}
struct Factory(Arc<HeldModel>);
impl ModelServiceFactory for Factory {
    fn create_model(
        &self,
        _: ModelServiceFactoryRequest<'_>,
    ) -> Result<ModelServiceBundle, ModelServiceFactoryError> {
        Ok(ModelServiceBundle::text_only(self.0.clone()))
    }
}
async fn start_work(runtime: &AssistantRuntime) -> SessionId {
    let provider = runtime
        .create_provider(CreateProviderRequest {
            connection: ProviderConnection {
                display_name: "Fixture".into(),
                provider_type: ProviderType::Local,
                endpoint: "https://example.invalid/v1".into(),
                protocol_preference: ProviderProtocolPreference::ChatCompletions,
                models_path: "/v1/models".into(),
                discovery_format: ModelDiscoveryFormat::OpenAi,
            },
            credential: ProviderCredentialChange::Clear,
        })
        .await
        .unwrap();
    let selection = ModelSelection {
        provider_instance_id: provider.provider_instance_id,
        model_id: "fixture".into(),
    };
    runtime
        .save_model_fixed_config(SaveModelFixedConfigRequest {
            selection: selection.clone(),
            origin: ModelConfigOrigin::Manual,
            parameters: ModelParameters {
                context_window_tokens: ModelTokenLimit::Known(32768.try_into().unwrap()),
                max_output_tokens: ModelTokenLimit::Known(4096.try_into().unwrap()),
                streaming: ModelFeatureSupport::Supported,
                tool_calls: ModelFeatureSupport::Supported,
                tool_choice: ModelToolChoiceSupport {
                    auto: ModelFeatureSupport::Supported,
                    none: ModelFeatureSupport::Supported,
                    required: ModelFeatureSupport::Supported,
                    named: ModelFeatureSupport::Supported,
                },
                image_input: ModelFeatureSupport::Unsupported,
                reasoning: ModelFeatureSupport::Unsupported,
                reasoning_mode: ModelReasoningMode::Unsupported,
                tool_image_projection: ModelToolImageProjection::Unsupported,
                ..Default::default()
            },
        })
        .await
        .unwrap();
    runtime
        .set_default_model(SetDefaultModelRequest {
            selection: Some(selection),
        })
        .await
        .unwrap();
    let id = runtime
        .create_session(CreateSessionRequest {
            title: Some("Fixture".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .session
        .session_id;
    runtime
        .submit_session_input(assistant_runtime::SubmitSessionInputRequest {
            source: assistant_runtime::InputChannelSource::desktop_text(),
            input: SubmitInputRequest {
                session_id: id.clone(),
                message: "wait".into(),
                variant: AgentVariant::default(),
                mode: SubmitInputMode::Normal,
                attachment_ids: vec![],
                quotes: vec![],
                skill_name: None,
                mcp_server_key: None,
                idempotency_key: None,
            },
        })
        .await
        .unwrap();
    id
}
async fn stopped(runtime: &AssistantRuntime) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while runtime.lifecycle().unwrap() != RuntimeLifecycle::Stopped {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn token_expiry_retains_work_then_recycles_and_logout_cancels_only_its_owner() {
    let model = HeldModel::new();
    let mut f = Fixture::with_domains(true, Some(Arc::new(Factory(model.clone())))).await;
    let alice = f.login("alice").await;
    let bob = f.login("bob").await;
    let a = f.services(&alice).await;
    let b = f.services(&bob).await;
    start_work(&a.runtime).await;
    model.started().await;
    start_work(&b.runtime).await;
    model.started().await;
    let latest = f.login("alice").await;
    assert_eq!(
        f.request(reqwest::Method::POST, "/auth/password", Some(&latest))
            .json(&json!({"old_password":"password1","new_password":"changed1"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert!(a.runtime.has_background_work());
    f.state.access.credentials.expire_for_test(&bob);
    assert!(b.runtime.has_background_work());
    assert!(
        f.state
            .access
            .credentials
            .domain_access(f.permit(&alice).user_key().unwrap())
            .is_ok()
    );
    let logout = f
        .request(reqwest::Method::POST, "/auth/logout", Some(&alice))
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::OK);
    assert_eq!(a.runtime.lifecycle().unwrap(), RuntimeLifecycle::Stopped);
    assert_eq!(b.runtime.lifecycle().unwrap(), RuntimeLifecycle::Running);
    assert!(b.runtime.has_background_work());
    model.release.cancel();
    stopped(&b.runtime).await;
    assert!(f.state.access.credentials.enterprise_users().is_empty());
    f.stop().await;
}

#[tokio::test]
async fn unavailable_center_stops_work_but_retains_login_for_explicit_recovery() {
    let model = HeldModel::new();
    let mut f = Fixture::with_domains(true, Some(Arc::new(Factory(model.clone())))).await;
    let alice = f.login("alice").await;
    let a = f.services(&alice).await;
    let session = start_work(&a.runtime).await;
    model.started().await;
    f.mock.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(
        f.session(&alice).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    stopped(&a.runtime).await;
    assert!(f.state.access.credentials.authenticate(&alice).is_some());
    f.mock.unavailable.store(false, Ordering::SeqCst);
    assert_eq!(f.ready(&alice).await.status(), StatusCode::NO_CONTENT);
    let reopened = f.services(&alice).await;
    assert!(!Arc::ptr_eq(&a, &reopened));
    assert!(
        reopened
            .runtime
            .get_session_view(GetSessionViewRequest {
                session_id: session
            })
            .await
            .is_ok()
    );
    assert!(!reopened.runtime.has_background_work());
    f.stop().await;
}

#[tokio::test]
async fn expired_client_work_is_still_rechecked_by_the_center_supervisor() {
    let model = HeldModel::new();
    let mut f = Fixture::with_domains(true, Some(Arc::new(Factory(model.clone())))).await;
    let alice = f.login("alice").await;
    let a = f.services(&alice).await;
    start_work(&a.runtime).await;
    model.started().await;
    f.state.access.credentials.expire_for_test(&alice);
    f.mock.tokens.lock().unwrap().clear();
    let credentials = f.state.access.credentials.clone();
    let stop = f.shutdown.clone();
    let checker = tokio::spawn(async move { credentials.recheck_connections(stop).await });
    stopped(&a.runtime).await;
    assert!(f.state.access.credentials.enterprise_users().is_empty());
    f.stop().await;
    checker.await.unwrap();
}

/// 全新非生产夹具先关闭 worker、独立复制并逐表核对，再占用 SQLite 排他锁模拟慢初始化。
async fn hold_database(f: &Fixture) -> rusqlite::Connection {
    let home = f.home.path().join(format!("users/{CENTER_ID}_1"));
    let store = crate::storage::LocalRuntimeStore::open(&home, 32)
        .await
        .unwrap();
    assistant_runtime::RuntimeStore::shutdown(&store)
        .await
        .unwrap();
    let path = home.join("data/runtime.sqlite3");
    let backup = home.join("data/initialization-fixture-backup.sqlite3");
    std::fs::copy(&path, &backup).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let saved =
        rusqlite::Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let tables = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for table in tables {
        let query = format!("SELECT COUNT(*) FROM \"{}\"", table.replace('"', "\"\""));
        let source: i64 = connection.query_row(&query, [], |row| row.get(0)).unwrap();
        let target: i64 = saved.query_row(&query, [], |row| row.get(0)).unwrap();
        assert_eq!(source, target, "backup table {table}");
    }
    connection
        .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
        .unwrap();
    connection
}

#[tokio::test]
async fn disconnected_initialization_waiter_does_not_own_the_initialization_task() {
    let mut f = Fixture::with_domains(true, None).await;
    let alice = f.login("alice").await;
    let connection = hold_database(&f).await;
    let domains = f.state.domains.clone().unwrap();
    let permit = f.permit(&alice);
    let first = tokio::spawn(async move { domains.ensure(&permit).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!first.is_finished());
    first.abort();
    let _ = first.await;
    connection.execute_batch("ROLLBACK").unwrap();
    drop(connection);
    assert_eq!(f.ready(&alice).await.status(), StatusCode::NO_CONTENT);
    let services = f.services(&alice).await;
    assert!(Arc::ptr_eq(&services, &f.services(&alice).await));
    f.stop().await;
}

#[tokio::test]
async fn center_outage_interrupts_readiness_without_reporting_a_lost_login() {
    let mut f = Fixture::with_domains(true, None).await;
    let alice = f.login("alice").await;
    let connection = hold_database(&f).await;
    let request = f.request(reqwest::Method::POST, "/runtime/ensure-ready", Some(&alice));
    let waiting = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiting.is_finished());
    f.mock.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(
        f.session(&alice).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        waiting.await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(f.state.access.credentials.authenticate(&alice).is_some());
    connection.execute_batch("ROLLBACK").unwrap();
    drop(connection);
    f.mock.unavailable.store(false, Ordering::SeqCst);
    assert_eq!(f.ready(&alice).await.status(), StatusCode::NO_CONTENT);
    f.stop().await;
}

#[tokio::test]
async fn logout_during_initialization_does_not_resurrect_old_requests_or_block_new_login() {
    let mut f = Fixture::with_domains(true, None).await;
    let alice = f.login("alice").await;
    let connection = hold_database(&f).await;
    let domains = f.state.domains.clone().unwrap();
    let permit = f.permit(&alice);
    let first = tokio::spawn(async move { domains.ensure(&permit).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!first.is_finished());
    let logout = f.request(reqwest::Method::POST, "/auth/logout", Some(&alice));
    let logging_out = tokio::spawn(async move { logout.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(2), async {
        while f.state.access.credentials.authenticate(&alice).is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let latest = f.login("alice").await;
    assert!(first.await.unwrap().is_err());
    connection.execute_batch("ROLLBACK").unwrap();
    drop(connection);
    assert_eq!(logging_out.await.unwrap().status(), StatusCode::OK);
    assert_eq!(f.ready(&latest).await.status(), StatusCode::NO_CONTENT);
    assert!(f.state.access.credentials.authenticate(&latest).is_some());
    f.stop().await;
}
