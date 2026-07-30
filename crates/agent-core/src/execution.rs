//! 执行句柄与启动入口。
//!
//! [`AgentExecution::start`] 后由**单一 `tokio` 任务**驱动状态机（[`crate::engine`]），
//! 无后台辅助任务。调用方得到事件流、完成结果与取消控制三个独立句柄：
//!
//! - 事件流：普通事件使用 bounded mpsc + `try_send`，唯一终态由独立 oneshot
//!   可靠交付；订阅断开不影响执行；
//! - 完成结果：oneshot 接收端 Future，drop 接收端不影响执行；
//! - 取消控制：只取消本次执行（`start` 内创建 `context.cancellation` 的
//!   子令牌，父级取消自动传播到本执行，反向不成立）。

use std::{future::Future, pin::Pin};

use agent_types::AssistantMessage;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::{
    AgentEventStream, ExecutionContext, ExecutionError, ExecutionInput, ExecutionSpec,
    engine::Engine, event::agent_event_channel,
};

/// 一次执行的完成 Future；解析为 [`ExecutionOutcome`]。
///
/// 语义上是 oneshot 接收端：引擎任务结束时恰好发送一次结果；drop 本 Future
/// 不影响执行本身。
pub type CompletionFuture = Pin<Box<dyn Future<Output = ExecutionOutcome> + Send>>;

/// 请求 Runtime 执行上下文压缩的原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionReason {
    /// 最近完整模型结果的窗口占用达到配置阈值。
    ThresholdReached,
    /// Provider 在请求建立前或模型流内明确报告上下文超限。
    ProviderOverflow,
}

/// 一次执行的最终结果；与四个终态事件镜像。
///
/// `ExecutionCompleted{message}` ↔ [`ExecutionOutcome::Completed`]、
/// `ExecutionFailed{error}` ↔ [`ExecutionOutcome::Failed`]、
/// `ExecutionCancelled` ↔ [`ExecutionOutcome::Cancelled`]、
/// `ExecutionCompactionRequired{reason,step}` ↔
/// [`ExecutionOutcome::CompactionRequired`]。每次执行恰好收敛到一个终态，
/// 完成结果与终态事件承载同一事实。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ExecutionOutcome {
    /// 正常完成，携带最终聚合的规范响应（最终消息 Core 不落账，经此交 Runtime）。
    Completed(AssistantMessage),
    /// 受控失败（模型失败、落账失败、预算到达）。
    Failed(ExecutionError),
    /// 已取消；收敛前所有未结算 Tool Call 已补记 interrupted 错误 `ToolResult`。
    Cancelled,
    /// 当前执行在指定 Model Step 前或期间需要由 Runtime 压缩上下文。
    CompactionRequired {
        /// 触发压缩交接的原因。
        reason: CompactionReason,
        /// 阈值预检即将开始或 Provider Overflow 已经开始的 Model Step。
        step: u32,
    },
}

/// 一次执行的取消控制句柄。
///
/// 只取消本次执行：内部持有 `context.cancellation` 的子令牌，父级令牌取消会
/// 传播到本执行，本句柄取消不影响父级与同级的其他执行。
#[derive(Clone, Debug)]
pub struct ExecutionControl {
    cancellation: CancellationToken,
}

impl ExecutionControl {
    /// 取消本次执行；幂等。
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}

/// 一次正在进行的 Agent 执行。
///
/// 不分配业务 ID，不携带 `RunId`/`SessionId`；Run 关联由 Runtime 在外围完成。
pub struct AgentExecution {
    /// 执行事件流；首事件 `ExecutionStarted`，恰好以一个终态事件结束。
    pub events: AgentEventStream,
    /// 完成结果；drop 不影响执行。
    pub completion: CompletionFuture,
    /// 取消控制。
    pub control: ExecutionControl,
}

impl AgentExecution {
    /// 启动一次 Agent 执行：冻结规格与输入，spawn 唯一驱动任务后立即返回。
    pub fn start(
        spec: ExecutionSpec,
        input: ExecutionInput,
        context: ExecutionContext,
    ) -> AgentExecution {
        let (sender, events) = agent_event_channel();
        // 子令牌：父级取消自动传播；ExecutionControl 只取消本执行。
        let cancellation = context.cancellation.child_token();
        let control = ExecutionControl {
            cancellation: cancellation.clone(),
        };
        let (completion_tx, completion_rx) = oneshot::channel::<ExecutionOutcome>();
        tokio::spawn(async move {
            let outcome = Engine::new(spec, input, context, cancellation, sender)
                .run()
                .await;
            // 接收端被 drop 时发送失败；执行已结束，丢弃结果即可。
            let _ = completion_tx.send(outcome);
        });
        let completion = Box::pin(async move {
            // 结构不变量：状态机收敛到唯一终态后必先发送结果再退出任务；
            // 发送失败的唯一原因（接收端 drop）不会导致接收端 await 出错。
            completion_rx
                .await
                .expect("engine task always resolves an outcome before exiting")
        });
        AgentExecution {
            events,
            completion,
            control,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        pin::Pin,
        sync::{Arc, Mutex},
        task::{Context, Poll},
    };

    use agent_context::ContextWindowEvaluator;
    use agent_model::{
        ModelCallContext, ModelCapabilities, ModelError, ModelEvent, ModelEventStream,
        ModelRequest, ModelService, ModelStreamFuture,
    };
    use agent_types::{
        ConversationMessage, ConversationSnapshot, FinishReason, MessageId, ModelIdentity, PartId,
        ProviderId, TextPart, UserMessage, UserPart,
    };
    use futures_core::Stream;
    use futures_util::StreamExt;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{
        AgentEvent, AllowAllAuthorizer, BudgetKind, ConversationDelta, ExchangeReceipt,
        ExecutionBudget, ExecutionRecorder, RecordFuture,
    };

    /// 记录 delta 的最小 Recorder（agent-core 不允许依赖 agent-testkit）。
    struct ListRecorder {
        deltas: Mutex<Vec<ConversationDelta>>,
        pending: Mutex<Option<AssistantMessage>>,
    }

    impl ListRecorder {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                deltas: Mutex::new(vec![]),
                pending: Mutex::new(None),
            })
        }
    }

    impl ExecutionRecorder for ListRecorder {
        fn begin_tool_exchange<'a>(
            &'a self,
            assistant: AssistantMessage,
        ) -> RecordFuture<'a, ExchangeReceipt> {
            Box::pin(async move {
                *self.pending.lock().expect("lock pending") = Some(assistant);
                ExchangeReceipt::new("exchange_1")
            })
        }

        fn complete_tool_exchange<'a>(
            &'a self,
            _receipt: &'a ExchangeReceipt,
            results: Vec<agent_types::ToolMessage>,
        ) -> RecordFuture<'a, ()> {
            Box::pin(async move {
                let assistant = self
                    .pending
                    .lock()
                    .expect("lock pending")
                    .take()
                    .ok_or_else(|| crate::RecordError {
                        message: "missing pending exchange".to_owned(),
                    })?;
                let mut deltas = self.deltas.lock().expect("lock deltas");
                deltas.push(ConversationDelta::Assistant(assistant));
                deltas.extend(results.into_iter().map(ConversationDelta::Tool));
                Ok(())
            })
        }
    }

    /// 桩模型：纯文本完成或建立前失败；观察取消令牌。
    struct StubModel {
        capabilities: ModelCapabilities,
        behavior: StubBehavior,
    }

    enum StubBehavior {
        Complete(AssistantMessage),
        FailEstablishment(ModelError),
    }

    impl ModelService for StubModel {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            128_000
        }

        fn stream(
            &self,
            _request: ModelRequest,
            context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            let behavior = match &self.behavior {
                StubBehavior::Complete(message) => Ok(vec![
                    ModelEvent::TurnStarted {
                        message_id: message.id.clone(),
                        model: message.model.clone(),
                    },
                    ModelEvent::TurnFinished {
                        message: message.clone(),
                    },
                ]),
                StubBehavior::FailEstablishment(error) => Err(error.clone()),
            };
            Box::pin(async move {
                if context.cancellation.is_cancelled() {
                    return Err(ModelError::Cancelled);
                }
                let events = behavior?;
                Ok(Box::pin(StubStream {
                    events: events.into(),
                }) as ModelEventStream)
            })
        }
    }

    struct StubStream {
        events: VecDeque<ModelEvent>,
    }

    impl Stream for StubStream {
        type Item = ModelEvent;

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<ModelEvent>> {
            Poll::Ready(self.events.pop_front())
        }
    }

    fn capabilities() -> ModelCapabilities {
        ModelCapabilities {
            reasoning: false,
            tool_calls: true,
            streaming: true,
        }
    }

    fn text_message(text: &str) -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new("message_1").expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("deepseek").expect("valid provider id"),
                "deepseek-reasoner",
            ),
            parts: vec![agent_types::AssistantPart::Text(TextPart {
                id: PartId::new("text_1").expect("valid part id"),
                text: text.to_owned(),
            })],
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    fn spec(model: impl ModelService + 'static) -> ExecutionSpec {
        ExecutionSpec {
            instructions: vec!["You are a helpful assistant.".to_owned()],
            model: Arc::new(model),
            context_window: Arc::new(ContextWindowEvaluator::new(0.8).expect("valid threshold")),
            tools: Default::default(),
            budget: ExecutionBudget::default(),
        }
    }

    fn input() -> ExecutionInput {
        ExecutionInput {
            conversation: ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
                id: MessageId::new("message_u1").expect("valid message id"),
                parts: vec![UserPart::Text(TextPart {
                    id: PartId::new("text_u1").expect("valid part id"),
                    text: "Hello.".to_owned(),
                })],
            })]),
        }
    }

    fn context(cancellation: CancellationToken) -> (ExecutionContext, Arc<ListRecorder>) {
        let recorder = ListRecorder::new();
        (
            ExecutionContext {
                cancellation,
                recorder: recorder.clone(),
                authorizer: Arc::new(AllowAllAuthorizer),
            },
            recorder,
        )
    }

    async fn collect_events(stream: AgentEventStream) -> Vec<AgentEvent> {
        stream.collect().await
    }

    #[test]
    fn outcome_round_trips_serde() {
        let outcomes = vec![
            ExecutionOutcome::Completed(text_message("done")),
            ExecutionOutcome::Failed(ExecutionError::BudgetExceeded {
                kind: BudgetKind::Steps,
                limit: 4,
            }),
            ExecutionOutcome::Cancelled,
            ExecutionOutcome::CompactionRequired {
                reason: CompactionReason::ThresholdReached,
                step: 2,
            },
        ];
        for outcome in outcomes {
            let json = serde_json::to_string(&outcome).expect("serialize outcome");
            assert_eq!(
                serde_json::from_str::<ExecutionOutcome>(&json).expect("deserialize outcome"),
                outcome
            );
        }
        // 稳定 tag：蛇形命名。
        let json = serde_json::to_value(ExecutionOutcome::Cancelled).expect("serialize to value");
        assert_eq!(json, serde_json::json!({"type": "cancelled"}));
    }

    #[tokio::test]
    async fn plain_text_execution_completes_with_event_lifecycle() {
        let model = StubModel {
            capabilities: capabilities(),
            behavior: StubBehavior::Complete(text_message("Hi there.")),
        };
        let (context, recorder) = context(CancellationToken::new());
        let execution = AgentExecution::start(spec(model), input(), context);

        let outcome = execution.completion.await;
        let ExecutionOutcome::Completed(message) = outcome else {
            panic!("expected Completed, got {outcome:?}");
        };
        assert_eq!(message, text_message("Hi there."));

        let events = collect_events(execution.events).await;
        assert_eq!(
            events,
            vec![
                AgentEvent::ExecutionStarted,
                AgentEvent::StepStarted { step: 1 },
                AgentEvent::ExecutionCompleted {
                    message: text_message("Hi there."),
                    dropped_events: 0,
                },
            ]
        );
        // 纯文本路径不产生任何落账增量（最终消息 Core 不落账）。
        assert!(recorder.deltas.lock().expect("lock deltas").is_empty());
    }

    #[tokio::test]
    async fn establishment_failure_resolves_failed_outcome() {
        let model = StubModel {
            capabilities: capabilities(),
            behavior: StubBehavior::FailEstablishment(ModelError::Auth("bad key".to_owned())),
        };
        let (context, _) = context(CancellationToken::new());
        let execution = AgentExecution::start(spec(model), input(), context);

        let outcome = execution.completion.await;
        assert_eq!(
            outcome,
            ExecutionOutcome::Failed(ExecutionError::Model(ModelError::Auth(
                "bad key".to_owned()
            )))
        );
        let events = collect_events(execution.events).await;
        assert_eq!(
            events,
            vec![
                AgentEvent::ExecutionStarted,
                AgentEvent::StepStarted { step: 1 },
                AgentEvent::ExecutionFailed {
                    error: ExecutionError::Model(ModelError::Auth("bad key".to_owned())),
                    dropped_events: 0,
                },
            ]
        );
    }

    #[tokio::test]
    async fn pre_cancelled_parent_token_converges_to_cancelled() {
        let model = StubModel {
            capabilities: capabilities(),
            behavior: StubBehavior::Complete(text_message("never reached")),
        };
        let parent = CancellationToken::new();
        parent.cancel();
        let (context, _) = context(parent);
        let execution = AgentExecution::start(spec(model), input(), context);

        // 父级取消经子令牌传播：建立前取消，收敛为唯一取消终态。
        let outcome = execution.completion.await;
        assert_eq!(outcome, ExecutionOutcome::Cancelled);
        let events = collect_events(execution.events).await;
        assert_eq!(
            events,
            vec![
                AgentEvent::ExecutionStarted,
                AgentEvent::StepStarted { step: 1 },
                AgentEvent::ExecutionCancelled { dropped_events: 0 },
            ]
        );
    }

    #[tokio::test]
    async fn dropped_completion_receiver_does_not_affect_execution() {
        let model = StubModel {
            capabilities: capabilities(),
            behavior: StubBehavior::Complete(text_message("still runs")),
        };
        let (context, _) = context(CancellationToken::new());
        let execution = AgentExecution::start(spec(model), input(), context);
        drop(execution.completion);

        // 完成接收端已 drop：执行照常收敛，终态事件照常发出。
        let events = collect_events(execution.events).await;
        assert!(matches!(
            events.last(),
            Some(AgentEvent::ExecutionCompleted { .. })
        ));
    }

    #[tokio::test]
    async fn dropped_event_stream_does_not_affect_completion() {
        let model = StubModel {
            capabilities: capabilities(),
            behavior: StubBehavior::Complete(text_message("no subscriber")),
        };
        let (context, _) = context(CancellationToken::new());
        let execution = AgentExecution::start(spec(model), input(), context);
        drop(execution.events);

        let outcome = execution.completion.await;
        assert!(matches!(outcome, ExecutionOutcome::Completed(_)));
    }
}
