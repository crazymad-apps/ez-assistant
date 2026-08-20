//! 单个模型 Turn 中 Tool Call 批次的执行与可靠结算。
//!
//! 本模块是 [`Engine`] 的私有子流程：它只消费已冻结 ToolSet 和已经 resolve 的批次，
//! 不拥有 Agent Loop，也不引入 Runtime 子任务概念。默认调用逐项串行；只有至少两个
//! 连续、显式标记为 `ParallelEligible` 的合法调用才形成并行组。

use std::{num::NonZeroU32, sync::Arc};

use agent_tools::{
    Dispatcher, ResolvedBatchItemRef, ResolvedToolBatch, ToolContext, ToolExecutionMode,
    ToolJsonFuture, ToolOutputChunk, ToolOutputSink,
};
use agent_types::{
    ConversationMessage, MessageId, ToolCall, ToolCallId, ToolMessage, ToolResult,
    ToolResultContent, ToolResultStatus,
};
use futures_util::future::join_all;

use super::Engine;
use crate::{
    ActiveGuardrailMode, AgentEvent, BudgetKind, ExchangeReceipt, ExecutionError, ExecutionOutcome,
    GuardrailKind, RecordError, ToolAuthorization, ToolCompletionStatus,
    guardrail::GuardrailTrigger,
};

/// 取消收敛时为未结算调用补记的模型可读错误文本。
const INTERRUPTED_TEXT: &str = "interrupted: execution cancelled";

/// 一个 Tool Call 批次的结局。
pub(super) enum BatchEnd {
    /// 整批正常结算（含 Deny/工具失败转换的错误结果），按批次顺序排列。
    Settled(Vec<ToolResult>),
    /// 执行已收敛到终态（Guardrail、取消或预算到达）。
    Terminal(ExecutionOutcome),
}

impl Engine {
    /// 逐位置结算一个已整批 resolve 的 Tool Call 批次。
    ///
    /// 本函数只负责识别串行位置和显式并行组；单位置的授权、执行、结果观察与各种
    /// 终态收敛分别由下层函数负责，避免 Agent Loop 和批次细节混在同一主流程中。
    pub(super) async fn execute_batch(
        &mut self,
        exchange: &ExchangeReceipt,
        calls: &[ToolCall],
        resolved: &mut ResolvedToolBatch,
    ) -> BatchEnd {
        let mut results = Vec::with_capacity(calls.len());
        let mut index = 0;
        while index < calls.len() {
            if self.cancellation.is_cancelled() {
                return BatchEnd::Terminal(
                    self.converge_cancelled(exchange, &calls[index..], results)
                        .await,
                );
            }

            let parallel_end = parallel_group_end(resolved, index);
            if parallel_end >= index + 2 {
                if let Some(outcome) = self
                    .execute_parallel_group(
                        exchange,
                        calls,
                        resolved,
                        index,
                        parallel_end,
                        &mut results,
                    )
                    .await
                {
                    return BatchEnd::Terminal(outcome);
                }
                index = parallel_end;
                continue;
            }

            if let Some(outcome) = self
                .execute_serial_position(exchange, calls, resolved, index, &mut results)
                .await
            {
                return BatchEnd::Terminal(outcome);
            }
            index += 1;
        }
        BatchEnd::Settled(results)
    }

    /// 结算一个不属于并行组的位置；Invalid 与缺失项只形成错误结果，不进入授权。
    async fn execute_serial_position(
        &mut self,
        exchange: &ExchangeReceipt,
        calls: &[ToolCall],
        resolved: &mut ResolvedToolBatch,
        index: usize,
        results: &mut Vec<ToolResult>,
    ) -> Option<ExecutionOutcome> {
        let call = &calls[index];
        let Some(item) = resolved.get(index) else {
            self.guardrails.reset_repeated_invocation();
            let result = error_result(
                &call.id,
                format!("resolved batch is missing position {index}"),
            );
            return self
                .settle_result(exchange, calls, index + 1, results, result)
                .await;
        };
        let invocation = match item {
            ResolvedBatchItemRef::Invalid(result) => {
                self.guardrails.reset_repeated_invocation();
                return self
                    .settle_result(exchange, calls, index + 1, results, result.clone())
                    .await;
            }
            ResolvedBatchItemRef::Valid(invocation) => invocation,
        };

        if let Some(trigger) = self.observe_invocation(invocation.fingerprint()) {
            self.emit_guardrail_trigger(trigger, &call.id);
            if trigger.mode == ActiveGuardrailMode::Enforce {
                let settled = std::mem::take(results);
                return Some(
                    self.enforce_guardrail(exchange, calls, index, settled, trigger)
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
            let settled = std::mem::take(results);
            return Some(
                self.converge_cancelled(exchange, &calls[index..], settled)
                    .await,
            );
        };

        match authorization {
            ToolAuthorization::Deny { reason } => {
                let result = error_result(&call.id, reason);
                self.settle_result(exchange, calls, index + 1, results, result)
                    .await
            }
            ToolAuthorization::Allow => {
                self.execute_authorized_serial(exchange, calls, resolved, index, results)
                    .await
            }
        }
    }

    /// 在授权通过后执行一个串行调用；预算与 started 都是 Tool SPI 前的硬边界。
    async fn execute_authorized_serial(
        &mut self,
        exchange: &ExchangeReceipt,
        calls: &[ToolCall],
        resolved: &mut ResolvedToolBatch,
        index: usize,
        results: &mut Vec<ToolResult>,
    ) -> Option<ExecutionOutcome> {
        let call = &calls[index];
        if let Some(limit) = self.spec.budget.max_tool_calls
            && self.dispatched >= limit
        {
            let settled = std::mem::take(results);
            return Some(
                self.converge_budget_exceeded(exchange, calls, index, settled, limit)
                    .await,
            );
        }

        if self
            .context
            .recorder
            .mark_tool_execution_started(exchange, &call.id)
            .await
            .is_err()
        {
            let result = error_result(
                &call.id,
                "tool execution start could not be recorded".to_owned(),
            );
            return self
                .settle_result(exchange, calls, index + 1, results, result)
                .await;
        }

        self.dispatched += 1;
        self.events.send(AgentEvent::ToolStarted {
            call_id: call.id.clone(),
        });
        let context = ToolContext::new(self.cancellation.clone(), self.output_sink(&call.id))
            .with_call_id(call.id.clone());
        // Tool SPI 要求取消后完成资源清理再解析 future；Engine 不直接 drop dispatch。
        let result = match Dispatcher::execute(resolved, index, context) {
            Ok(execution) => execution.await,
            Err(error) => error_result(&call.id, error.to_string()),
        };
        if self.cancellation.is_cancelled() {
            let settled = std::mem::take(results);
            return Some(
                self.converge_cancelled(exchange, &calls[index..], settled)
                    .await,
            );
        }

        self.settle_result(exchange, calls, index + 1, results, result)
            .await
    }

    /// 同时授权并执行一段连续、显式允许并行的调用。
    ///
    /// 组内先按原顺序完成 Guardrail 预检，再并发授权；所有执行 future 都在预算和
    /// started 可靠点完成后一起 poll。结果最终仍按原 Tool Call 顺序观察并回填。
    async fn execute_parallel_group(
        &mut self,
        exchange: &ExchangeReceipt,
        calls: &[ToolCall],
        resolved: &mut ResolvedToolBatch,
        start: usize,
        end: usize,
        results: &mut Vec<ToolResult>,
    ) -> Option<ExecutionOutcome> {
        for index in start..end {
            let Some(ResolvedBatchItemRef::Valid(invocation)) = resolved.get(index) else {
                return Some(self.fail(ExecutionError::Internal));
            };
            if let Some(trigger) = self.observe_invocation(invocation.fingerprint()) {
                self.emit_guardrail_trigger(trigger, &calls[index].id);
                if trigger.mode == ActiveGuardrailMode::Enforce {
                    let settled = std::mem::take(results);
                    return Some(
                        self.enforce_guardrail(exchange, calls, start, settled, trigger)
                            .await,
                    );
                }
            }
        }

        let authorizer = self.context.authorizer.clone();
        let authorization = async {
            join_all((start..end).map(|index| {
                let invocation = match resolved.get(index) {
                    Some(ResolvedBatchItemRef::Valid(invocation)) => invocation,
                    _ => unreachable!("parallel group was validated before authorization"),
                };
                authorizer.authorize(invocation, resolved)
            }))
            .await
        };
        let authorizations = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => {
                let settled = std::mem::take(results);
                return Some(
                    self.converge_cancelled(exchange, &calls[start..], settled)
                        .await,
                );
            }
            authorizations = authorization => authorizations,
        };

        let mut group_results = vec![None; end - start];
        let mut executions: Vec<(usize, ToolJsonFuture<'static>)> = Vec::new();
        let mut budget_exceeded = None;
        for (offset, authorization) in authorizations.into_iter().enumerate() {
            let index = start + offset;
            let call = &calls[index];
            match authorization {
                ToolAuthorization::Deny { reason } => {
                    group_results[offset] = Some(error_result(&call.id, reason));
                }
                ToolAuthorization::Allow => {
                    if let Some(limit) = self.spec.budget.max_tool_calls
                        && self.dispatched >= limit
                    {
                        budget_exceeded.get_or_insert((offset, limit));
                        group_results[offset] = Some(error_result(
                            &call.id,
                            format!("tool call budget exceeded (limit {limit})"),
                        ));
                        continue;
                    }
                    if self
                        .context
                        .recorder
                        .mark_tool_execution_started(exchange, &call.id)
                        .await
                        .is_err()
                    {
                        group_results[offset] = Some(error_result(
                            &call.id,
                            "tool execution start could not be recorded".to_owned(),
                        ));
                        continue;
                    }

                    // Future 在这里只被构造，尚未 poll；先完成整组预算和 started 边界。
                    self.dispatched += 1;
                    self.events.send(AgentEvent::ToolStarted {
                        call_id: call.id.clone(),
                    });
                    let context =
                        ToolContext::new(self.cancellation.clone(), self.output_sink(&call.id))
                            .with_call_id(call.id.clone());
                    match Dispatcher::execute(resolved, index, context) {
                        Ok(execution) => executions.push((offset, execution)),
                        Err(error) => {
                            group_results[offset] = Some(error_result(&call.id, error.to_string()));
                        }
                    }
                }
            }
        }

        for (offset, result) in join_all(
            executions
                .into_iter()
                .map(|(offset, execution)| async move { (offset, execution.await) }),
        )
        .await
        {
            group_results[offset] = Some(result);
        }

        // 所有 Tool SPI 已完成取消清理后，整组才统一结算为 interrupted。
        if self.cancellation.is_cancelled() {
            let settled = std::mem::take(results);
            return Some(
                self.converge_cancelled(exchange, &calls[start..], settled)
                    .await,
            );
        }

        let mut ordered = Vec::with_capacity(group_results.len());
        for (offset, result) in group_results.into_iter().enumerate() {
            let call = &calls[start + offset];
            let result = result.unwrap_or_else(|| {
                error_result(
                    &call.id,
                    "parallel tool call did not produce a result".to_owned(),
                )
            });
            self.events.send(AgentEvent::ToolCompleted {
                call_id: call.id.clone(),
                status: completion_status(&result),
            });
            ordered.push(result);
        }
        results.extend(ordered.iter().cloned());

        // 预算终态优先：只观察首次预算超限前已经发生的结果。
        let guardrail_result_count = budget_exceeded
            .map(|(offset, _)| offset)
            .unwrap_or(ordered.len());
        for (offset, result) in ordered.iter().take(guardrail_result_count).enumerate() {
            if let Some(trigger) = self.observe_result(result.status.clone()) {
                self.emit_guardrail_trigger(trigger, &calls[start + offset].id);
                if trigger.mode == ActiveGuardrailMode::Enforce {
                    let settled = std::mem::take(results);
                    return Some(
                        self.enforce_guardrail(exchange, calls, end, settled, trigger)
                            .await,
                    );
                }
            }
        }
        if let Some((_, limit)) = budget_exceeded {
            let settled = std::mem::take(results);
            return Some(
                self.converge_budget_exceeded(exchange, calls, end, settled, limit)
                    .await,
            );
        }
        None
    }

    /// 将一个普通结果加入有序结果集，并执行结果型 Guardrail 观察。
    async fn settle_result(
        &mut self,
        exchange: &ExchangeReceipt,
        calls: &[ToolCall],
        unsettled_from: usize,
        results: &mut Vec<ToolResult>,
        result: ToolResult,
    ) -> Option<ExecutionOutcome> {
        let call_id = result.call_id.clone();
        self.events.send(AgentEvent::ToolCompleted {
            call_id: call_id.clone(),
            status: completion_status(&result),
        });
        let status = result.status.clone();
        results.push(result);
        let trigger = self.observe_result(status)?;
        self.emit_guardrail_trigger(trigger, &call_id);
        if trigger.mode != ActiveGuardrailMode::Enforce {
            return None;
        }
        let settled = std::mem::take(results);
        Some(
            self.enforce_guardrail(exchange, calls, unsettled_from, settled, trigger)
                .await,
        )
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

    /// Enforce 时为尚未结算的位置补齐错误，先原子完成 exchange，再发失败终态。
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

    /// 为未结算调用补记 interrupted，完成 pending exchange 后归为取消终态。
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

    /// 为当前及后续调用补记预算错误，完整落账后归为预算失败终态。
    async fn converge_budget_exceeded(
        &mut self,
        exchange: &ExchangeReceipt,
        calls: &[ToolCall],
        unsettled_from: usize,
        mut results: Vec<ToolResult>,
        limit: u32,
    ) -> ExecutionOutcome {
        for call in calls.iter().skip(unsettled_from) {
            results.push(error_result(
                &call.id,
                format!("tool call budget exceeded (limit {limit})"),
            ));
            self.events.send(AgentEvent::ToolCompleted {
                call_id: call.id.clone(),
                status: ToolCompletionStatus::Failed,
            });
        }
        if let Err(error) = self.complete_tool_results(exchange, results).await {
            return self.fail(ExecutionError::Record(error));
        }
        self.fail(ExecutionError::BudgetExceeded {
            kind: BudgetKind::ToolCalls,
            limit,
        })
    }

    /// 构造完整有序 ToolMessage 批次，原子完成 pending exchange，再追加投影。
    pub(super) async fn complete_tool_results(
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
}

/// 返回从 `start` 开始的连续显式并行组末端（exclusive）。
fn parallel_group_end(resolved: &ResolvedToolBatch, start: usize) -> usize {
    let mut end = start;
    while matches!(
        resolved.get(end),
        Some(ResolvedBatchItemRef::Valid(invocation))
            if invocation.execution_mode() == ToolExecutionMode::ParallelEligible
    ) {
        end += 1;
    }
    end
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

fn completion_status(result: &ToolResult) -> ToolCompletionStatus {
    match result.status {
        ToolResultStatus::Success => ToolCompletionStatus::Success,
        ToolResultStatus::Error => ToolCompletionStatus::Failed,
    }
}

fn error_result(call_id: &ToolCallId, message: String) -> ToolResult {
    ToolResult {
        call_id: call_id.clone(),
        status: ToolResultStatus::Error,
        content: ToolResultContent::text(message),
        metadata: None,
    }
}

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
