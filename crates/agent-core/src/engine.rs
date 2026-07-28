//! Agent Loop 状态机。
//!
//! 语义层状态机（功能设计 §4.3）：
//!
//! ```text
//! Preparing → BuildingContext → StreamingModel
//!   →（无工具调用 → Completed）
//!   → RecordingAssistantMessage → Authorizing → ExecutingTools
//!   → RecordingToolResults → BuildingContext …
//! ```
//!
//! 终态为 Completed / Failed / Cancelled 之一且唯一。关键纪律：
//!
//! - step 从 1 开始计数；`max_steps` 在每次**模型调用前**预检（`Some(0)` 时
//!   一次模型调用都不发生），`max_tool_calls` 在每次 **dispatch 前**预检，
//!   两者都是副作用前的硬边界；预算按"实际 dispatch 数"计，授权 `Deny` 不计入。
//! - 副作用前顺序：begin pending exchange → 逐 call 独立过闸 → 工具执行；
//!   整批 ToolResult 原子完成 exchange 后才进入下一轮。
//! - 取消收敛：模型流内经 `ModelCallContext` 传播并在终态收敛点检查令牌、
//!   授权等待经 `select!` race、工具执行等待 cancellation-aware dispatch 完成清理；
//!   收敛前为批次内未结算调用补记 interrupted 错误 `ToolResult` 并落账，
//!   journal 中 Tool Call/Result 始终配对。
//! - 最终消息（无工具调用的完成 Turn）Core **不落账**，经完成事件交 Runtime。

use std::sync::Arc;

use agent_model::{
    GenerationConfig, LifecycleValidator, ModelCallContext, ModelError, ModelEvent, ModelRequest,
    ProviderOptions,
};
use agent_tools::{Dispatcher, ToolContext, ToolOutputChunk, ToolOutputSink};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, MessageId,
    ToolCall, ToolCallId, ToolChoice, ToolMessage, ToolResult, ToolResultContent, ToolResultStatus,
};
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::{
    AgentEvent, BudgetKind, ExchangeReceipt, ExecutionContext, ExecutionError, ExecutionInput,
    ExecutionOutcome, ExecutionSpec, RecordError, ToolAuthorization, ToolCompletionStatus,
    event::AgentEventSender,
};

/// 取消收敛时为未结算调用补记的模型可读错误文本。
const INTERRUPTED_TEXT: &str = "interrupted: execution cancelled";

/// 引擎状态机；由 [`crate::AgentExecution::start`] 内唯一的 tokio 任务驱动。
pub(crate) struct Engine {
    spec: ExecutionSpec,
    context: ExecutionContext,
    /// 执行级取消令牌（`context.cancellation` 的子令牌）。
    cancellation: CancellationToken,
    events: AgentEventSender,
    /// 对话投影：输入快照 + 本轮用户输入 + 每轮追加的 Assistant/Tool 消息。
    projection: Vec<ConversationMessage>,
    /// 已开始的模型 Turn 数（`max_steps` 预检基数）。
    steps: u32,
    /// 已实际 dispatch 的工具调用数（`max_tool_calls` 预检基数；Deny 不计入）。
    dispatched: u32,
    /// 已产生的 ToolMessage 数（`toolmsg_{n}` 确定性序号基数）。
    tool_messages: u32,
}

/// 一个模型 Turn 的结局。
enum TurnEnd {
    /// 模型 Turn 正常聚合出完整消息。
    Finished(AssistantMessage),
    /// 执行已收敛到终态（Failed/Cancelled），直接作为执行结果返回。
    Terminal(ExecutionOutcome),
}

impl Engine {
    /// 从执行事实源组装状态机；投影初始为输入快照消息 + 本轮用户输入。
    pub(crate) fn new(
        spec: ExecutionSpec,
        input: ExecutionInput,
        context: ExecutionContext,
        cancellation: CancellationToken,
        events: AgentEventSender,
    ) -> Self {
        let ExecutionInput {
            conversation,
            user_input,
        } = input;
        let mut projection = conversation.messages;
        projection.push(ConversationMessage::User(user_input));
        Self {
            spec,
            context,
            cancellation,
            events,
            projection,
            steps: 0,
            dispatched: 0,
            tool_messages: 0,
        }
    }

    /// 驱动状态机直到唯一终态；返回与终态事件镜像的执行结果。
    pub(crate) async fn run(mut self) -> ExecutionOutcome {
        self.events.send(AgentEvent::ExecutionStarted);
        loop {
            // max_steps 预检：模型调用前的硬边界；Some(0) 时一次调用都不发生。
            if let Some(limit) = self.spec.budget.max_steps
                && self.steps >= limit
            {
                return self.fail(ExecutionError::BudgetExceeded {
                    kind: BudgetKind::Steps,
                    limit,
                });
            }
            // step 从 1 开始。
            self.steps += 1;
            self.events
                .send(AgentEvent::StepStarted { step: self.steps });

            let request = self.build_request();
            let message = match self.stream_turn(request).await {
                TurnEnd::Finished(message) => message,
                TurnEnd::Terminal(outcome) => return outcome,
            };

            let calls = tool_calls_of(&message);
            if calls.is_empty() {
                // 无工具调用：最终消息 Core 不落账，经完成事件交 Runtime。
                self.events.send(AgentEvent::ExecutionCompleted {
                    message: message.clone(),
                    dropped_events: self.events.dropped_events(),
                });
                return ExecutionOutcome::Completed(message);
            }

            // 副作用前顺序第一步：持久化 pending tool exchange。
            // Err 阻断后续一切副作用（无任何授权与工具执行）。
            let exchange = match self
                .context
                .recorder
                .begin_tool_exchange(message.clone())
                .await
            {
                Ok(receipt) => receipt,
                Err(error) => return self.fail(ExecutionError::Record(error)),
            };
            self.projection
                .push(ConversationMessage::Assistant(message));

            // 按批次顺序宣告全部 ToolProposed，再逐 call 独立过闸处理。
            for call in &calls {
                self.events
                    .send(AgentEvent::ToolProposed { call: call.clone() });
            }
            match self.execute_batch(&exchange, &calls).await {
                BatchEnd::Settled(results) => {
                    // 整批结算完：原子完成 exchange 并追加投影，然后进入下一轮。
                    if let Err(error) = self.complete_tool_results(&exchange, results).await {
                        return self.fail(ExecutionError::Record(error));
                    }
                }
                BatchEnd::Terminal(outcome) => return outcome,
            }
        }
    }

    /// 组装一次模型 Turn 的规范请求（纯机械投影，无隐藏策略）。
    fn build_request(&self) -> ModelRequest {
        ModelRequest {
            system: self.spec.instructions.clone(),
            conversation: ConversationSnapshot::new(self.projection.clone()),
            tools: self.spec.tools.definitions().to_vec(),
            tool_choice: ToolChoice::Auto,
            generation: GenerationConfig::default(),
            reasoning: None,
            provider_options: ProviderOptions::new(),
        }
    }

    /// 执行一个模型 Turn：建立流、桥接 delta 事件、等待唯一终态。
    ///
    /// 取消经 `ModelCallContext` 传播给模型服务；本函数不 race，只在终态收敛点
    /// 检查令牌（服务契约保证取消后流以受控终态结束）。
    async fn stream_turn(&mut self, request: ModelRequest) -> TurnEnd {
        let established = self
            .spec
            .model
            .stream(request, ModelCallContext::new(self.cancellation.clone()))
            .await;
        let stream = match established {
            Ok(stream) => stream,
            // 建立前失败；已取消或 Cancelled 错误归取消收敛。
            Err(error) => return TurnEnd::Terminal(self.model_failure(error).await),
        };
        let mut stream = LifecycleValidator::new(stream);
        while let Some(event) = stream.next().await {
            match event {
                ModelEvent::TextDelta { id, delta } => {
                    self.events.send(AgentEvent::TextDelta { id, delta });
                }
                ModelEvent::ReasoningDelta { id, delta } => {
                    self.events.send(AgentEvent::ReasoningDelta { id, delta });
                }
                ModelEvent::TurnFinished { message } => return TurnEnd::Finished(message),
                ModelEvent::TurnFailed { error } => {
                    return TurnEnd::Terminal(self.model_failure(error).await);
                }
                // TurnStarted、Part Started/Finished、ToolCall*、UsageUpdated
                // 没有对应的 AgentEvent，只在最终消息与契约校验中体现。
                _ => {}
            }
        }
        // LifecycleValidator 保证恰一个终态，正常不可达；保持状态机 total。
        TurnEnd::Terminal(self.fail(ExecutionError::Model(ModelError::Protocol(
            "model stream ended without a terminal event".to_owned(),
        ))))
    }

    /// 模型失败收敛：执行令牌已取消或错误为 `ModelError::Cancelled` 时归
    /// `ExecutionCancelled`（此时没有已宣告的 Tool Call，收敛无需补记）；
    /// 其余归 `ExecutionFailed{Model}`。
    async fn model_failure(&mut self, error: ModelError) -> ExecutionOutcome {
        if self.cancellation.is_cancelled() || matches!(error, ModelError::Cancelled) {
            self.cancelled()
        } else {
            self.fail(ExecutionError::Model(error))
        }
    }

    /// 逐 call 独立过闸并顺序执行一个批次的 Tool Call。
    ///
    /// 正常路径结算出与批次等长、同序的 `ToolResult` 列表；取消或预算到达时
    /// 直接收敛到终态（未结算调用已先行结算错误 `ToolResult`）。
    async fn execute_batch(&mut self, exchange: &ExchangeReceipt, calls: &[ToolCall]) -> BatchEnd {
        let mut results: Vec<ToolResult> = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            // 授权等待与取消 race（biased：取消优先，保证 race 可断言）。
            let authorization = tokio::select! {
                biased;
                () = self.cancellation.cancelled() => None,
                authorization = self.context.authorizer.authorize(call, calls) => Some(authorization),
            };
            let Some(authorization) = authorization else {
                return BatchEnd::Terminal(
                    self.converge_cancelled(exchange, &calls[index..], results)
                        .await,
                );
            };
            match authorization {
                ToolAuthorization::Deny { reason } => {
                    // Deny 在授权闸处转换为错误 ToolResult：不执行、不计入预算、循环继续。
                    results.push(error_result(&call.id, reason));
                    self.events.send(AgentEvent::ToolCompleted {
                        call_id: call.id.clone(),
                        status: ToolCompletionStatus::Failed,
                    });
                }
                ToolAuthorization::Allow => {
                    // max_tool_calls 预检：dispatch 前的硬边界，按实际 dispatch 数计。
                    if let Some(limit) = self.spec.budget.max_tool_calls
                        && self.dispatched >= limit
                    {
                        // 本 call 及批次内剩余调用全部结算预算超额错误，
                        // 整批落账后受控终止。
                        for pending in &calls[index..] {
                            results.push(error_result(
                                &pending.id,
                                format!("tool call budget exceeded (limit {limit})"),
                            ));
                            self.events.send(AgentEvent::ToolCompleted {
                                call_id: pending.id.clone(),
                                status: ToolCompletionStatus::Failed,
                            });
                        }
                        if let Err(error) = self.complete_tool_results(exchange, results).await {
                            return BatchEnd::Terminal(self.fail(ExecutionError::Record(error)));
                        }
                        return BatchEnd::Terminal(self.fail(ExecutionError::BudgetExceeded {
                            kind: BudgetKind::ToolCalls,
                            limit,
                        }));
                    }
                    self.dispatched += 1;
                    self.events.send(AgentEvent::ToolStarted {
                        call_id: call.id.clone(),
                    });
                    let context =
                        ToolContext::new(self.cancellation.clone(), self.output_sink(&call.id));
                    // Tool SPI 要求取消后完成资源清理再解析 future；Engine 不直接
                    // drop dispatch，否则真实 Shell 的进程树清理可能被跳过。
                    let result = Dispatcher::dispatch(&self.spec.tools, call, context).await;
                    if self.cancellation.is_cancelled() {
                        return BatchEnd::Terminal(
                            self.converge_cancelled(exchange, &calls[index..], results)
                                .await,
                        );
                    }
                    self.events.send(AgentEvent::ToolCompleted {
                        call_id: call.id.clone(),
                        status: completion_status(&result),
                    });
                    results.push(result);
                }
            }
        }
        BatchEnd::Settled(results)
    }

    /// 取消收敛：为批次内未结算调用补记 interrupted 错误 `ToolResult`（保证
    /// Tool Call/Result 配对），按序落账全部已结算 ToolMessage 后归为
    /// `ExecutionCancelled` 唯一终态；此处落账失败仍按阻断规则收敛为
    /// `ExecutionFailed{Record}`。
    async fn converge_cancelled(
        &mut self,
        exchange: &ExchangeReceipt,
        unsettled: &[ToolCall],
        mut results: Vec<ToolResult>,
    ) -> ExecutionOutcome {
        for call in unsettled {
            results.push(error_result(&call.id, INTERRUPTED_TEXT.to_owned()));
            self.events.send(AgentEvent::ToolCompleted {
                call_id: call.id.clone(),
                status: ToolCompletionStatus::Failed,
            });
        }
        if let Err(error) = self.complete_tool_results(exchange, results).await {
            return self.fail(ExecutionError::Record(error));
        }
        self.cancelled()
    }

    /// 构造完整有序 ToolMessage 批次，原子完成 pending exchange，再追加投影。
    ///
    /// `ToolMessage.id` 为执行内确定性序号 `toolmsg_{n}`，从 1 开始，跨批次递增。
    async fn complete_tool_results(
        &mut self,
        exchange: &ExchangeReceipt,
        results: Vec<ToolResult>,
    ) -> Result<(), RecordError> {
        let mut messages = Vec::with_capacity(results.len());
        for result in results {
            self.tool_messages += 1;
            messages.push(ToolMessage {
                id: MessageId::new(format!("toolmsg_{}", self.tool_messages))
                    .expect("tool message id is never empty"),
                result,
            });
        }
        self.context
            .recorder
            .complete_tool_exchange(exchange, messages.clone())
            .await?;
        for message in messages {
            self.projection.push(ConversationMessage::Tool(message));
        }
        Ok(())
    }

    /// 工具流式输出桥接：`ToolOutputChunk` → `AgentEvent::ToolOutput`。
    fn output_sink(&self, call_id: &ToolCallId) -> ToolOutputSink {
        let events = self.events.clone();
        let call_id = call_id.clone();
        Arc::new(move |chunk: ToolOutputChunk| {
            events.send(AgentEvent::ToolOutput {
                call_id: call_id.clone(),
                channel: chunk.channel,
                chunk: chunk.delta,
            });
        })
    }

    /// 收敛为 `ExecutionFailed`：发送终态事件（携带丢弃计数）并返回镜像结果。
    fn fail(&self, error: ExecutionError) -> ExecutionOutcome {
        self.events.send(AgentEvent::ExecutionFailed {
            error: error.clone(),
            dropped_events: self.events.dropped_events(),
        });
        ExecutionOutcome::Failed(error)
    }

    /// 收敛为取消终态；仅在没有 pending exchange 或 exchange 已完整完成后调用。
    fn cancelled(&self) -> ExecutionOutcome {
        self.events.send(AgentEvent::ExecutionCancelled {
            dropped_events: self.events.dropped_events(),
        });
        ExecutionOutcome::Cancelled
    }
}

/// 一个 Tool Call 批次的结局。
enum BatchEnd {
    /// 整批正常结算（含 Deny/工具失败转换的错误结果），按批次顺序排列。
    Settled(Vec<ToolResult>),
    /// 执行已收敛到终态（取消或预算到达）。
    Terminal(ExecutionOutcome),
}

/// 提取消息中的全部 Tool Call（保持 parts 内顺序）。
fn tool_calls_of(message: &AssistantMessage) -> Vec<ToolCall> {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .collect()
}

/// 由 `ToolResultStatus` 映射完成状态。
fn completion_status(result: &ToolResult) -> ToolCompletionStatus {
    match result.status {
        ToolResultStatus::Success => ToolCompletionStatus::Success,
        ToolResultStatus::Error => ToolCompletionStatus::Failed,
    }
}

/// 构造模型可读文本内容的错误 `ToolResult`（Deny/预算/取消结算共用）。
fn error_result(call_id: &ToolCallId, message: String) -> ToolResult {
    ToolResult {
        call_id: call_id.clone(),
        status: ToolResultStatus::Error,
        content: ToolResultContent::Text(message),
    }
}
