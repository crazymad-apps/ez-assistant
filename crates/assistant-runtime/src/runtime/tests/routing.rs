//! 验证通用 owner 路由真正进入每次模型调用，不借用 Trace、Run 或历史绑定。
use super::*;

pub(super) struct RoutingModelFactory {
    model: Arc<dyn ModelService>,
    pub(super) calls: Arc<Mutex<Vec<(String, String)>>>,
}
impl RoutingModelFactory {
    pub(super) fn new(model: Arc<dyn ModelService>) -> Self {
        Self {
            model,
            calls: Default::default(),
        }
    }
}
struct RoutedModel {
    inner: Arc<dyn ModelService>,
    route: String,
    model: String,
    calls: Arc<Mutex<Vec<(String, String)>>>,
}
impl ModelService for RoutedModel {
    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }
    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }
    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        self.calls
            .lock()
            .unwrap()
            .push((self.route.clone(), self.model.clone()));
        self.inner.stream(request, context)
    }
}
impl ModelServiceFactory for RoutingModelFactory {
    fn create_model(
        &self,
        request: ModelServiceFactoryRequest<'_>,
    ) -> Result<ModelServiceBundle, ModelServiceFactoryError> {
        Ok(ModelServiceBundle::text_only(Arc::new(RoutedModel {
            inner: self.model.clone(),
            route: request
                .conversation_routing_id
                .expect("all product calls have a route")
                .to_owned(),
            model: request.model.to_owned(),
            calls: self.calls.clone(),
        })))
    }
}

fn fixture_model() -> Arc<dyn ModelService> {
    Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        (0..12).map(|i| {
            ModelScript::Events(message_events(&assistant_text(
                &format!("routing-answer-{i}"),
                "answer",
            )))
        }),
    ))
}

#[tokio::test]
async fn main_routing_survives_runs_summary_clear_and_runtime_restart_and_fork_is_distinct() {
    let factory = Arc::new(RoutingModelFactory::new(fixture_model()));
    let runtime = runtime_with_factories(
        factory.clone(),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        64,
    );
    let session = runtime
        .create_session(Default::default())
        .await
        .unwrap()
        .session
        .session_id;
    submit_completed_turn(&runtime, &session, "first").await;
    submit_completed_turn(&runtime, &session, "second").await;
    let expected = super::super::model::conversation_routing_id(
        &assistant_protocol::ConversationOwner::MainSession {
            session_id: session.clone(),
        },
    );
    let forked = runtime
        .fork_session(assistant_protocol::ForkSessionRequest {
            session_id: session.clone(),
            fork_point: assistant_protocol::MessageId::new("routing-answer-1").unwrap(),
            expected_generation: 1,
        })
        .await
        .unwrap()
        .session
        .session_id;
    submit_completed_turn(&runtime, &forked, "fork").await;
    let fork_route = factory.calls.lock().unwrap().last().unwrap().0.clone();
    assert_ne!(fork_route, expected);
    runtime
        .compact_session(assistant_protocol::CompactSessionRequest {
            session_id: session.clone(),
            operation_id: assistant_protocol::IdempotencyKey::new("routing-compact").unwrap(),
            expected_generation: 1,
        })
        .await
        .unwrap();
    assert_eq!(factory.calls.lock().unwrap().last().unwrap().0, expected);
    runtime
        .clear_session(assistant_protocol::ClearSessionRequest {
            session_id: session.clone(),
            operation_id: assistant_protocol::IdempotencyKey::new("routing-clear").unwrap(),
            expected_generation: 2,
        })
        .await
        .unwrap();
    submit_completed_turn(&runtime, &session, "after clear").await;
    assert_eq!(factory.calls.lock().unwrap().last().unwrap().0, expected);
    let store = runtime.store.clone();
    runtime.shutdown(Default::default()).await.unwrap();
    let mut restarted = runtime_with_store(
        fixture_model(),
        store,
        RuntimeConfig::new(NonZeroUsize::new(64).unwrap()),
    )
    .await;
    let restarted_factory = Arc::new(RoutingModelFactory::new(fixture_model()));
    restarted.model_factory = restarted_factory.clone();
    submit_completed_turn(&restarted, &session, "after restart").await;
    assert_eq!(restarted_factory.calls.lock().unwrap()[0].0, expected);
    restarted
        .validate_model_connection(configured_validation_request())
        .await
        .unwrap();
    restarted
        .validate_model_connection(configured_validation_request())
        .await
        .unwrap();
    let calls = restarted_factory.calls.lock().unwrap();
    assert_ne!(calls[1].0, expected);
    assert_ne!(calls[1].0, calls[2].0);
}

#[tokio::test]
async fn title_uses_the_main_owner_without_starting_a_run() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&assistant_text(
                "routing-title-answer",
                "answer",
            ))),
            ModelScript::Events(message_events(&assistant_title_tool_call(
                "routing-title",
                "routing-title-call",
                "Routing title",
            ))),
        ],
    ));
    let factory = Arc::new(RoutingModelFactory::new(model));
    let runtime = runtime_with_factories(
        factory.clone(),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        64,
    );
    let session = runtime
        .create_session(Default::default())
        .await
        .unwrap()
        .session
        .session_id;
    submit_completed_turn(&runtime, &session, "title source").await;
    runtime
        .generate_session_title(assistant_protocol::GenerateSessionTitleRequest {
            session_id: session.clone(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if runtime
                .get_session(assistant_protocol::GetSessionRequest {
                    session_id: session.clone(),
                })
                .await
                .unwrap()
                .session
                .title
                == "Routing title"
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let calls = factory.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, calls[1].0);
}
