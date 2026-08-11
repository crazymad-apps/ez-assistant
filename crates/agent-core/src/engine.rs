//! Agent Loop 状态机。
//!
//! 语义层状态机（功能设计 §4.3）：
//!
//! ```text
//! Preparing → BuildingContext → StreamingModel
//!   →（无工具调用 → Completed）
//!   → RecordingAssistantMessage → ResolvingToolInvocations → Authorizing → ExecutingTools
//!   → RecordingToolResults → BuildingContext …
//! ```
//!
//! 终态为 Completed / Failed / Cancelled / CompactionRequired 之一且唯一。关键纪律：
//!
//! - step 从 1 开始计数；`max_steps` 在每次**模型调用前**预检（`Some(0)` 时
//!   一次模型调用都不发生），`max_tool_calls` 在每次 **dispatch 前**预检，
//!   两者都是副作用前的硬边界；预算按"实际 dispatch 数"计，授权 `Deny` 不计入。
//! - 每个 Model Step 在 `StepStarted` 前调用共享 Context Window Evaluator；
//!   达到阈值或 Provider 报 Context Overflow 时以 CompactionRequired 交回 Runtime，
//!   Core 不在当前执行内压缩或重试。
//! - 副作用前顺序：begin pending exchange → resolve whole batch → Guardrail →
//!   逐个 valid invocation 独立过闸 → 工具执行；invalid item 不进入授权或执行。
//!   整批 ToolResult 原子完成 exchange 后才进入下一轮。
//! - 取消收敛：模型流内经 `ModelCallContext` 传播并在终态收敛点检查令牌、
//!   授权等待经 `select!` race、工具执行等待 cancellation-aware dispatch 完成清理；
//!   收敛前为批次内未结算调用补记 interrupted 错误 `ToolResult` 并落账，
//!   journal 中 Tool Call/Result 始终配对。
//! - 最终消息（无工具调用的完成 Turn）Core **不落账**，经完成事件交 Runtime。

use std::{num::NonZeroU32, sync::Arc};

use agent_context::ContextWindowDecision;
use agent_model::{LifecycleValidator, ModelCallContext, ModelError, ModelEvent, ModelRequest};
use agent_tools::{
    Dispatcher, ResolvedBatchItemRef, ResolvedToolBatch, ToolContext, ToolOutputChunk,
    ToolOutputSink,
};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, MessageId,
    ToolCall, ToolCallId, ToolMessage, ToolResult, ToolResultContent, ToolResultStatus,
};
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::{
    ActiveGuardrailMode, AgentEvent, BudgetKind, CompactionReason, ExchangeReceipt,
    ExecutionContext, ExecutionError, ExecutionInput, ExecutionOutcome, ExecutionSpec,
    GuardrailKind, RecordError, ToolAuthorization, ToolCompletionStatus, event::AgentEventSender,
    guardrail::GuardrailState, guardrail::GuardrailTrigger,
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
    /// 对话投影：完整输入快照 + 每轮追加的 Assistant/Tool 消息。
    projection: Vec<ConversationMessage>,
    /// 已开始的模型 Turn 数（`max_steps` 预检基数）。
    steps: u32,
    /// 已实际 dispatch 的工具调用数（`max_tool_calls` 预检基数；Deny 不计入）。
    dispatched: u32,
    /// 本次执行已尝试的 ToolMessage 序号，用于寻找 Conversation 中未占用的 `toolmsg_{n}`。
    tool_messages: u32,
    /// 仅在当前 AgentExecution 内保留的重复调用与连续失败状态。
    guardrails: GuardrailState,
}

/// 一个模型 Turn 的结局。
enum TurnEnd {
    /// 模型 Turn 正常聚合出完整消息。
    Finished(AssistantMessage),
    /// 执行已收敛到终态（Failed/Cancelled/CompactionRequired），直接返回。
    Terminal(ExecutionOutcome),
}

impl Engine {
    /// 从执行事实源组装状态机；投影初始为 Runtime 提供的完整输入快照。
    pub(crate) fn new(
        spec: ExecutionSpec,
        input: ExecutionInput,
        context: ExecutionContext,
        cancellation: CancellationToken,
        events: AgentEventSender,
    ) -> Self {
        let ExecutionInput { conversation } = input;
        let projection = conversation.messages;
        Self {
            spec,
            context,
            cancellation,
            events,
            projection,
            steps: 0,
            dispatched: 0,
            tool_messages: 0,
            guardrails: GuardrailState::default(),
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
            // next_step 从 1 开始；只有实际建立 Provider Turn 时才计入 steps。
            let next_step = self.steps + 1;
            let snapshot = ConversationSnapshot::new(self.projection.clone());
            let evaluation = match self
                .spec
                .context_window
                .evaluate(&snapshot, self.spec.model.as_ref())
            {
                Ok(evaluation) => evaluation,
                Err(error) => return self.fail(ExecutionError::ContextWindow(error)),
            };
            if evaluation.decision == ContextWindowDecision::CompactionRequired {
                return self.compaction_required(CompactionReason::ThresholdReached, next_step);
            }
            let request = self.build_request();
            self.steps = next_step;
            self.events
                .send(AgentEvent::StepStarted { step: self.steps });
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

            // 按批次顺序宣告全部 ToolProposed，再整批 resolve 并逐位置处理。
            for call in &calls {
                self.events
                    .send(AgentEvent::ToolProposed { call: call.clone() });
            }
            let mut resolved = Dispatcher::resolve_batch(&self.spec.tools, &calls);
            match self.execute_batch(&exchange, &calls, &mut resolved).await {
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
            system: self.spec.system_prompt.clone(),
            conversation: ConversationSnapshot::new(self.projection.clone()),
            tools: self.spec.tools.definitions().to_vec(),
            tool_choice: self.spec.model_request.tool_choice.clone(),
            generation: self.spec.model_request.generation.clone(),
            reasoning: self.spec.model_request.reasoning.clone(),
            provider_options: self.spec.model_request.provider_options.clone(),
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
        let mut latest_usage = None;
        while let Some(event) = stream.next().await {
            match event {
                ModelEvent::TextDelta { id, delta } => {
                    self.events.send(AgentEvent::TextDelta { id, delta });
                }
                ModelEvent::ReasoningDelta { id, delta } => {
                    self.events.send(AgentEvent::ReasoningDelta { id, delta });
                }
                ModelEvent::UsageUpdated { usage } => {
                    latest_usage = Some(usage);
                }
                ModelEvent::TurnFinished { message } => {
                    if let Some(usage) = message.usage.clone().or(latest_usage) {
                        self.events.send(AgentEvent::UsageUpdated {
                            step: self.steps,
                            usage,
                        });
                    }
                    return TurnEnd::Finished(message);
                }
                ModelEvent::TurnFailed { error } => {
                    return TurnEnd::Terminal(self.model_failure(error).await);
                }
                // TurnStarted、Part Started/Finished、ToolCall* 没有对应的
                // AgentEvent，只在最终消息与契约校验中体现。
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
    /// Provider Context Overflow 归压缩交接终态，其余归 `ExecutionFailed{Model}`。
    async fn model_failure(&mut self, error: ModelError) -> ExecutionOutcome {
        if self.cancellation.is_cancelled() || matches!(error, ModelError::Cancelled) {
            self.cancelled()
        } else if matches!(error, ModelError::ContextOverflow { .. }) {
            self.compaction_required(CompactionReason::ProviderOverflow, self.steps)
        } else {
            self.fail(ExecutionError::Model(error))
        }
    }

    /// 逐位置结算一个已整批 resolve 的 Tool Call 批次。
    ///
    /// 正常路径结算出与批次等长、同序的 `ToolResult` 列表；取消、预算到达或 Enforce
    /// Guardrail 触发时直接收敛到终态（未结算调用已先行结算错误 `ToolResult`）。
    async fn execute_batch(
        &mut self,
        exchange: &ExchangeReceipt,
        calls: &[ToolCall],
        resolved: &mut ResolvedToolBatch,
    ) -> BatchEnd {
        let mut results: Vec<ToolResult> = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            if self.cancellation.is_cancelled() {
                return BatchEnd::Terminal(
                    self.converge_cancelled(exchange, &calls[index..], results)
                        .await,
                );
            }
            let Some(item) = resolved.get(index) else {
                self.guardrails.reset_repeated_invocation();
                let result = error_result(
                    &call.id,
                    format!("resolved batch is missing position {index}"),
                );
                let status = result.status.clone();
                results.push(result);
                self.events.send(AgentEvent::ToolCompleted {
                    call_id: call.id.clone(),
                    status: ToolCompletionStatus::Failed,
                });
                if let Some(trigger) = self.observe_result(status) {
                    self.emit_guardrail_trigger(trigger, &call.id);
                    if trigger.mode == ActiveGuardrailMode::Enforce {
                        return BatchEnd::Terminal(
                            self.enforce_guardrail(exchange, calls, index + 1, results, trigger)
                                .await,
                        );
                    }
                }
                continue;
            };
            let invocation = match item {
                ResolvedBatchItemRef::Invalid(result) => {
                    self.guardrails.reset_repeated_invocation();
                    results.push(result.clone());
                    self.events.send(AgentEvent::ToolCompleted {
                        call_id: call.id.clone(),
                        status: ToolCompletionStatus::Failed,
                    });
                    if let Some(trigger) = self.observe_result(result.status.clone()) {
                        self.emit_guardrail_trigger(trigger, &call.id);
                        if trigger.mode == ActiveGuardrailMode::Enforce {
                            return BatchEnd::Terminal(
                                self.enforce_guardrail(
                                    exchange,
                                    calls,
                                    index + 1,
                                    results,
                                    trigger,
                                )
                                .await,
                            );
                        }
                    }
                    continue;
                }
                ResolvedBatchItemRef::Valid(invocation) => invocation,
            };
            if let Some(trigger) = self.observe_invocation(invocation.fingerprint()) {
                self.emit_guardrail_trigger(trigger, &call.id);
                if trigger.mode == ActiveGuardrailMode::Enforce {
                    return BatchEnd::Terminal(
                        self.enforce_guardrail(exchange, calls, index, results, trigger)
                            .await,
                    );
                }
            }
            // 授权等待与取消 race（biased：取消优先，保证 race 可断言）。
            let authorization = tokio::select! {
                biased;
                () = self.cancellation.cancelled() => None,
                authorization = self.context.authorizer.authorize(invocation, resolved) => Some(authorization),
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
                    let result = error_result(&call.id, reason);
                    self.events.send(AgentEvent::ToolCompleted {
                        call_id: call.id.clone(),
                        status: ToolCompletionStatus::Failed,
                    });
                    let status = result.status.clone();
                    results.push(result);
                    if let Some(trigger) = self.observe_result(status) {
                        self.emit_guardrail_trigger(trigger, &call.id);
                        if trigger.mode == ActiveGuardrailMode::Enforce {
                            return BatchEnd::Terminal(
                                self.enforce_guardrail(
                                    exchange,
                                    calls,
                                    index + 1,
                                    results,
                                    trigger,
                                )
                                .await,
                            );
                        }
                    }
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
                    let result = match Dispatcher::execute(resolved, index, context) {
                        Ok(execution) => execution.await,
                        Err(error) => error_result(&call.id, error.to_string()),
                    };
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
                    let status = result.status.clone();
                    results.push(result);
                    if let Some(trigger) = self.observe_result(status) {
                        self.emit_guardrail_trigger(trigger, &call.id);
                        if trigger.mode == ActiveGuardrailMode::Enforce {
                            return BatchEnd::Terminal(
                                self.enforce_guardrail(
                                    exchange,
                                    calls,
                                    index + 1,
                                    results,
                                    trigger,
                                )
                                .await,
                            );
                        }
                    }
                }
            }
        }
        BatchEnd::Settled(results)
    }

    fn observe_invocation(
        &mut self,
        fingerprint: &agent_tools::ToolFingerprint,
    ) -> Option<GuardrailTrigger> {
        let config = self
            .spec
            .guardrails
            .as_ref()
            .and_then(|guardrails| guardrails.repeated_invocation);
        self.guardrails.observe_invocation(config, fingerprint)
    }

    fn observe_result(&mut self, status: ToolResultStatus) -> Option<GuardrailTrigger> {
        let config = self
            .spec
            .guardrails
            .as_ref()
            .and_then(|guardrails| guardrails.consecutive_failures);
        self.guardrails.observe_result(config, status)
    }

    fn emit_guardrail_trigger(&self, trigger: GuardrailTrigger, call_id: &ToolCallId) {
        self.events.send(AgentEvent::GuardrailTriggered {
            kind: trigger.kind,
            mode: trigger.mode,
            threshold: trigger.threshold,
            observed: trigger.observed,
            call_id: call_id.clone(),
        });
    }

    /// Enforce 时为尚未结算的位置补齐 Guardrail 错误，先原子完成 pending exchange，
    /// 再发可靠失败终态；若 complete 失败，Record error 优先。
    async fn enforce_guardrail(
        &mut self,
        exchange: &ExchangeReceipt,
        calls: &[ToolCall],
        unsettled_from: usize,
        mut results: Vec<ToolResult>,
        trigger: GuardrailTrigger,
    ) -> ExecutionOutcome {
        for call in calls.iter().skip(unsettled_from) {
            results.push(guardrail_error_result(
                &call.id,
                trigger.kind,
                trigger.threshold,
            ));
            self.events.send(AgentEvent::ToolCompleted {
                call_id: call.id.clone(),
                status: ToolCompletionStatus::Failed,
            });
        }
        if let Err(error) = self.complete_tool_results(exchange, results).await {
            return self.fail(ExecutionError::Record(error));
        }
        self.fail(ExecutionError::GuardrailTriggered {
            kind: trigger.kind,
            threshold: trigger.threshold,
        })
    }

    /// 取消收敛：为批次内未结算调用补记 interrupted 错误 `ToolResult`（保证
    /// Tool Call/Result 配对），按序落账全部已结算 ToolMessage 后归为
    /// `ExecutionCancelled` 唯一终态；此处落账失败仍按阻断规则收敛为
    /// `ExecutionFailed{Record}`，不能在 Tool Call/Result 尚未配对时宣告取消成功。
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
    /// `ToolMessage.id` 从 `toolmsg_1` 开始寻找当前完整 Conversation 中未占用的序号。
    ///
    /// 每个 Run 都会创建新的 Engine，因此不能只依赖执行内计数器：历史 Run 可能已经
    /// 持久化同名 ToolMessage。生成前必须对完整投影去重，否则第二个工具 Run 会在
    /// Store 的 Conversation 全局 Message ID 校验处永久留下 ready exchange。
    async fn complete_tool_results(
        &mut self,
        exchange: &ExchangeReceipt,
        results: Vec<ToolResult>,
    ) -> Result<(), RecordError> {
        let mut messages = Vec::with_capacity(results.len());
        for result in results {
            messages.push(ToolMessage {
                id: self.next_tool_message_id()?,
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

    fn next_tool_message_id(&mut self) -> Result<MessageId, RecordError> {
        loop {
            self.tool_messages = self
                .tool_messages
                .checked_add(1)
                .ok_or_else(|| RecordError {
                    message: "tool message id sequence is exhausted".to_owned(),
                })?;
            let candidate =
                MessageId::new(format!("toolmsg_{}", self.tool_messages)).map_err(|_| {
                    RecordError {
                        message: "tool message id could not be constructed".to_owned(),
                    }
                })?;
            if self
                .projection
                .iter()
                .all(|message| conversation_message_id(message) != &candidate)
            {
                return Ok(candidate);
            }
        }
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

    /// 收敛为上下文压缩交接终态；Core 不发起压缩，也不重试当前 Step。
    fn compaction_required(&self, reason: CompactionReason, step: u32) -> ExecutionOutcome {
        self.events.send(AgentEvent::ExecutionCompactionRequired {
            reason,
            step,
            dropped_events: self.events.dropped_events(),
        });
        ExecutionOutcome::CompactionRequired { reason, step }
    }
}

fn conversation_message_id(message: &ConversationMessage) -> &MessageId {
    match message {
        ConversationMessage::System(message) => &message.id,
        ConversationMessage::ContextSummary(message) => &message.id,
        ConversationMessage::User(message) => &message.id,
        ConversationMessage::Assistant(message) => &message.id,
        ConversationMessage::Tool(message) => &message.id,
    }
}

/// 一个 Tool Call 批次的结局。
enum BatchEnd {
    /// 整批正常结算（含 Deny/工具失败转换的错误结果），按批次顺序排列。
    Settled(Vec<ToolResult>),
    /// 执行已收敛到终态（Guardrail、取消或预算到达）。
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

/// 构造 Enforce 为未执行位置补齐的模型可读错误结果。
fn guardrail_error_result(
    call_id: &ToolCallId,
    kind: GuardrailKind,
    threshold: NonZeroU32,
) -> ToolResult {
    error_result(
        call_id,
        format!(
            "guardrail enforced: {kind:?} reached threshold {}",
            threshold.get()
        ),
    )
}
