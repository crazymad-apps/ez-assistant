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
//! 终态为 Completed / Failed / Cancelled / CompactionRequired /
//! ContinuationRequired 之一且唯一。关键纪律：
//!
//! - step 默认从 1 开始，也可由上层一次性提供起始值；`max_steps` 在每次**模型调用前**预检（`Some(0)` 时
//!   一次模型调用都不发生），`max_tool_calls` 在每次 **dispatch 前**预检，
//!   两者都是副作用前的硬边界；预算按"实际 dispatch 数"计，授权 `Deny` 不计入。
//! - 每个 Model Step 在 `StepStarted` 前调用共享 Context Window Evaluator；
//!   达到阈值或 Provider 报 Context Overflow 时以 CompactionRequired 交回 Runtime，
//!   Core 不在当前执行内压缩或重试。
//! - 副作用前顺序：begin pending exchange → resolve whole batch → Guardrail →
//!   valid invocation 独立过闸 → 工具执行；默认工具逐项串行，只有连续且显式
//!   `ParallelEligible` 的调用组成并行组；invalid item 不进入授权或执行。
//!   整批 ToolResult 仍按原顺序原子完成 exchange 后才进入下一轮。
//! - 取消收敛：模型流内经 `ModelCallContext` 传播并在终态收敛点检查令牌、
//!   授权等待经 `select!` race、工具执行等待 cancellation-aware dispatch 完成清理；
//!   收敛前为批次内未结算调用补记 interrupted 错误 `ToolResult` 并落账，
//!   journal 中 Tool Call/Result 始终配对。
//! - 最终消息（无工具调用的完成 Turn）Core **不落账**，经完成事件交 Runtime。

use agent_context::ContextWindowDecision;
use agent_model::{LifecycleValidator, ModelCallContext, ModelError, ModelEvent, ModelRequest};
use agent_tools::Dispatcher;
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, ToolCall,
};
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::{
    AgentEvent, BudgetKind, CompactionReason, ContinuationReason, ExecutionConsumption,
    ExecutionContext, ExecutionError, ExecutionInput, ExecutionOutcome, ExecutionSpec,
    event::AgentEventSender, guardrail::GuardrailState,
};

mod tool_batch;

use tool_batch::BatchEnd;

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
    /// 本段第一个 Model Turn 使用的 Runtime Run 全局 step。
    starting_step: std::num::NonZeroU32,
    /// 已实际 dispatch 的工具调用数（`max_tool_calls` 预检基数；Deny 不计入）。
    dispatched: u32,
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
        starting_step: std::num::NonZeroU32,
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
            starting_step,
            dispatched: 0,
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
            // next_step 从冻结起始值开始；只有实际建立 Provider Turn 时才计入 steps。
            let Some(next_step) = self.starting_step.get().checked_add(self.steps) else {
                return self.fail(ExecutionError::Internal);
            };
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
            self.steps += 1;
            self.events
                .send(AgentEvent::StepStarted { step: next_step });
            let message = match self.stream_turn(request).await {
                TurnEnd::Finished(message) => message,
                TurnEnd::Terminal(outcome) => return outcome,
            };

            let calls = tool_calls_of(&message);
            if calls.is_empty() {
                // 无工具调用：最终消息 Core 不落账，经完成事件交 Runtime。
                self.events.send(AgentEvent::ExecutionCompleted {
                    step: next_step,
                    message: message.clone(),
                    dropped_events: self.events.dropped_events(),
                });
                return ExecutionOutcome::Completed {
                    step: next_step,
                    message,
                    consumption: self.consumption(),
                };
            }

            // 副作用前顺序第一步：持久化 pending tool exchange。
            // Err 阻断后续一切副作用（无任何授权与工具执行）。
            let exchange = match self
                .context
                .recorder
                .begin_tool_exchange(self.current_step(), message.clone())
                .await
            {
                Ok(receipt) => receipt,
                Err(error) => return self.fail(ExecutionError::Record(error)),
            };
            self.projection
                .push(ConversationMessage::Assistant(message));

            // 按批次顺序宣告全部 ToolProposed，再整批 resolve 并逐位置处理。
            for call in &calls {
                self.events.send(AgentEvent::ToolProposed {
                    step: self.current_step(),
                    call: call.clone(),
                });
            }
            let mut resolved = Dispatcher::resolve_batch(&self.spec.tools, &calls);
            match self.execute_batch(&exchange, &calls, &mut resolved).await {
                BatchEnd::Settled(results) => {
                    // 整批结算完：原子完成 exchange 并追加投影，然后进入下一轮。
                    match self.complete_tool_results(&exchange, results).await {
                        Ok(completion) if completion.continuation_required => {
                            return self.continuation_required();
                        }
                        Ok(_) => {}
                        Err(error) => return self.fail(ExecutionError::Record(error)),
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
                    self.events.send(AgentEvent::TextDelta {
                        step: self.current_step(),
                        id,
                        delta,
                    });
                }
                ModelEvent::ReasoningDelta { id, delta } => {
                    self.events.send(AgentEvent::ReasoningDelta {
                        step: self.current_step(),
                        id,
                        delta,
                    });
                }
                ModelEvent::UsageUpdated { usage } => {
                    latest_usage = Some(usage);
                }
                ModelEvent::TurnFinished { message } => {
                    if let Some(usage) = message.usage.clone().or(latest_usage) {
                        self.events.send(AgentEvent::UsageUpdated {
                            step: self.current_step(),
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
            self.compaction_required(CompactionReason::ProviderOverflow, self.current_step())
        } else {
            self.fail(ExecutionError::Model(error))
        }
    }

    /// 收敛为 `ExecutionFailed`：发送终态事件（携带丢弃计数）并返回镜像结果。
    fn fail(&self, error: ExecutionError) -> ExecutionOutcome {
        self.events.send(AgentEvent::ExecutionFailed {
            error: error.clone(),
            dropped_events: self.events.dropped_events(),
        });
        ExecutionOutcome::Failed {
            error,
            consumption: self.consumption(),
        }
    }

    /// 收敛为取消终态；仅在没有 pending exchange 或 exchange 已完整完成后调用。
    fn cancelled(&self) -> ExecutionOutcome {
        self.events.send(AgentEvent::ExecutionCancelled {
            dropped_events: self.events.dropped_events(),
        });
        ExecutionOutcome::Cancelled {
            consumption: self.consumption(),
        }
    }

    /// 收敛为上下文压缩交接终态；Core 不发起压缩，也不重试当前 Step。
    fn compaction_required(&self, reason: CompactionReason, step: u32) -> ExecutionOutcome {
        let consumption = self.consumption();
        self.events.send(AgentEvent::ExecutionCompactionRequired {
            reason,
            step,
            consumption,
            dropped_events: self.events.dropped_events(),
        });
        ExecutionOutcome::CompactionRequired {
            reason,
            step,
            consumption,
        }
    }

    /// 收敛为通用上下文改变交接终态；具体续跑编排由 Runtime 持有。
    fn continuation_required(&self) -> ExecutionOutcome {
        let reason = ContinuationReason::ContextChanged;
        let consumption = self.consumption();
        self.events.send(AgentEvent::ExecutionContinuationRequired {
            reason,
            consumption,
            dropped_events: self.events.dropped_events(),
        });
        ExecutionOutcome::ContinuationRequired {
            reason,
            consumption,
        }
    }

    /// 当前已开始 Model Turn 的 Run 全局 step。
    fn current_step(&self) -> u32 {
        debug_assert!(self.steps > 0, "current step requires a started model turn");
        self.starting_step
            .get()
            .checked_add(self.steps - 1)
            .expect("step overflow is rejected before a model turn starts")
    }

    /// 本段 execution 的可靠预算消费事实。
    fn consumption(&self) -> ExecutionConsumption {
        ExecutionConsumption {
            steps: self.steps,
            tool_calls: self.dispatched,
        }
    }
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
