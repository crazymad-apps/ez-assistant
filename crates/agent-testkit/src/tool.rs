//! 脚本化的 [`Tool`] 实现：成功、执行失败或挂起直到取消。
//!
//! 输入为任意 JSON（`serde_json::Value`，schemars 派生"任意值"schema），输出
//! 为脚本给定的 JSON；执行开始时可经 `ToolContext::output_sink` 回放流式输出
//! 片段（验证引擎的 `ToolOutput` 事件桥接）。每次执行写入共享
//! [`OrderLog`](crate::OrderLog)。

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use agent_tools::{
    Tool, ToolContext, ToolError, ToolExecuteFuture, ToolExecutionMode, ToolOutputChunk,
    ToolResolution,
};
use agent_types::ToolName;
use serde_json::Value;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::OrderLog;
use crate::order::LogEntry;

/// 脚本化的工具行为。
#[derive(Clone, Debug)]
enum ToolBehavior {
    /// 立即成功，输出序列化后进入单一 JSON Tool Result Part。
    Succeed(Value),
    /// 立即以执行错误失败（`ToolError::Execution`，转为错误 `ToolResult`）。
    Fail(String),
    /// 挂起直到取消令牌触发，完成资源清理后以执行错误返回。
    Hang,
}

/// 确定性工具执行闸门，用于证明多个 execution future 是否真正重叠。
///
/// 每个装配该闸门的工具进入 `execute` 后先增加计数，再挂起到 [`release`](Self::release)。
/// 测试可以等待指定进入数，不依赖 wall clock 或 sleep。
#[derive(Clone, Default)]
pub struct ToolExecutionGate {
    entered: Arc<AtomicUsize>,
    changed: Arc<Notify>,
    released: CancellationToken,
}

impl ToolExecutionGate {
    /// 创建未放行的执行闸门。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前已经进入闸门的工具数量。
    pub fn entered(&self) -> usize {
        self.entered.load(Ordering::SeqCst)
    }

    /// 等到至少 `count` 个工具进入；已达到时立即返回。
    pub async fn wait_for_entered(&self, count: usize) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.entered() >= count {
                return;
            }
            notified.await;
        }
    }

    /// 放行当前和后续进入的工具；幂等。
    pub fn release(&self) {
        self.released.cancel();
    }

    async fn enter(&self, cancellation: &CancellationToken) -> bool {
        self.entered.fetch_add(1, Ordering::SeqCst);
        self.changed.notify_waiters();
        tokio::select! {
            biased;
            () = cancellation.cancelled() => false,
            () = self.released.cancelled() => true,
        }
    }
}

/// 脚本化的确定性 Fake 工具。
#[derive(Clone)]
pub struct ScriptedTool {
    name: ToolName,
    description: String,
    behavior: ToolBehavior,
    execution_mode: ToolExecutionMode,
    execution_gate: Option<ToolExecutionGate>,
    output_chunks: Vec<ToolOutputChunk>,
    entered: Option<Arc<Notify>>,
    cleanup_completed: Option<Arc<Notify>>,
    resolved_input: Option<Value>,
    executed_inputs: Arc<Mutex<Vec<Value>>>,
    log: OrderLog,
}

impl ScriptedTool {
    /// 成功工具：输出给定的 JSON 值。
    pub fn succeed(name: &str, output: Value, log: OrderLog) -> Self {
        Self::new(name, ToolBehavior::Succeed(output), log)
    }

    /// 失败工具：以给定的执行错误消息失败。
    pub fn failing(name: &str, message: impl Into<String>, log: OrderLog) -> Self {
        Self::new(name, ToolBehavior::Fail(message.into()), log)
    }

    /// 挂起工具：执行挂起直到取消，并在返回前完成清理。
    pub fn hanging(name: &str, log: OrderLog) -> Self {
        Self::new(name, ToolBehavior::Hang, log)
    }

    fn new(name: &str, behavior: ToolBehavior, log: OrderLog) -> Self {
        Self {
            name: ToolName::new(name).expect("valid tool name"),
            description: format!("Scripted tool `{name}`"),
            behavior,
            execution_mode: ToolExecutionMode::Serial,
            execution_gate: None,
            output_chunks: vec![],
            entered: None,
            cleanup_completed: None,
            resolved_input: None,
            executed_inputs: Arc::new(Mutex::new(Vec::new())),
            log,
        }
    }

    /// 覆盖默认的工具描述。
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// 覆盖注册时冻结的内部执行属性。
    pub fn with_execution_mode(mut self, execution_mode: ToolExecutionMode) -> Self {
        self.execution_mode = execution_mode;
        self
    }

    /// 进入执行后挂起在确定性闸门，直到测试放行或执行被取消。
    pub fn with_execution_gate(mut self, gate: ToolExecutionGate) -> Self {
        self.execution_gate = Some(gate);
        self
    }

    /// 执行开始时经 `output_sink` 逐条回放的流式输出片段。
    pub fn with_output_chunks(mut self, chunks: Vec<ToolOutputChunk>) -> Self {
        self.output_chunks = chunks;
        self
    }

    /// 进入 `execute` 时发出一次通知（取消 race 测试的确定性同步点）。
    pub fn with_entered_signal(mut self, entered: Arc<Notify>) -> Self {
        self.entered = Some(entered);
        self
    }

    /// 取消后资源清理完成时发出通知。
    pub fn with_cleanup_signal(mut self, cleanup_completed: Arc<Notify>) -> Self {
        self.cleanup_completed = Some(cleanup_completed);
        self
    }

    /// 覆盖无副作用 resolve 的输出，供授权观察与执行一致性测试使用。
    pub fn with_resolved_input(mut self, resolved_input: Value) -> Self {
        self.resolved_input = Some(resolved_input);
        self
    }

    /// 返回 `execute` 实际消费过的 resolved input 快照。
    pub fn executed_inputs(&self) -> Vec<Value> {
        self.executed_inputs
            .lock()
            .expect("executed inputs mutex poisoned")
            .clone()
    }
}

impl Tool for ScriptedTool {
    type Input = Value;
    type ResolvedInput = Value;
    type Output = Value;

    fn name(&self) -> ToolName {
        self.name.clone()
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        self.execution_mode
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        Ok(ToolResolution::general(
            self.resolved_input.clone().unwrap_or(input),
        ))
    }

    fn execute<'a>(&'a self, input: Value, context: ToolContext) -> ToolExecuteFuture<'a, Value> {
        Box::pin(async move {
            self.executed_inputs
                .lock()
                .expect("executed inputs mutex poisoned")
                .push(input);
            self.log.push(LogEntry::ToolExecute {
                name: self.name.as_str().to_owned(),
            });
            if let Some(entered) = &self.entered {
                entered.notify_one();
            }
            if let Some(gate) = &self.execution_gate
                && !gate.enter(&context.cancellation).await
            {
                return Err(ToolError::execution("gated tool interrupted"));
            }
            for chunk in &self.output_chunks {
                (context.output_sink)(chunk.clone());
            }
            match &self.behavior {
                ToolBehavior::Succeed(output) => Ok(output.clone()),
                ToolBehavior::Fail(message) => Err(ToolError::execution(message.clone())),
                ToolBehavior::Hang => {
                    context.cancellation.cancelled().await;
                    self.log.push(LogEntry::ToolCleanup {
                        name: self.name.as_str().to_owned(),
                    });
                    if let Some(cleanup_completed) = &self.cleanup_completed {
                        cleanup_completed.notify_one();
                    }
                    Err(ToolError::execution("hung tool interrupted"))
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agent_tools::{Dispatcher, ResolvedBatchItemRef, ToolOutputChannel, ToolRegistry};
    use agent_types::{ToolCall, ToolCallId, ToolResultContent, ToolResultStatus};
    use serde_json::json;

    use super::*;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new("call_1").expect("valid call id"),
            name: ToolName::new(name).expect("valid tool name"),
            arguments: json!({"any": "thing"}),
        }
    }

    async fn dispatch(tool: ScriptedTool, call: &ToolCall) -> agent_types::ToolResult {
        let mut registry = ToolRegistry::new();
        registry.register(tool).expect("register scripted tool");
        let snapshot = registry.snapshot();
        let mut batch = Dispatcher::resolve_batch(&snapshot, std::slice::from_ref(call));
        match batch.get(0) {
            Some(ResolvedBatchItemRef::Invalid { result, .. }) => result.clone(),
            Some(ResolvedBatchItemRef::Valid(_)) => {
                Dispatcher::execute(&mut batch, 0, ToolContext::default())
                    .expect("single-item batch index")
                    .await
            }
            None => panic!("single-item batch"),
        }
    }

    #[tokio::test]
    async fn succeed_tool_returns_json_output_and_logs_execution() {
        let log = OrderLog::new();
        let tool = ScriptedTool::succeed("get_date", json!({"date": "2026-07-27"}), log.clone());
        let result = dispatch(tool, &call("get_date")).await;
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(
            result.content,
            ToolResultContent::json(json!({"date": "2026-07-27"}))
        );
        assert_eq!(
            log.entries(),
            vec![LogEntry::ToolExecute {
                name: "get_date".to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn failing_tool_maps_to_error_result() {
        let log = OrderLog::new();
        let tool = ScriptedTool::failing("explode", "boom", log);
        let result = dispatch(tool, &call("explode")).await;
        assert_eq!(result.status, ToolResultStatus::Error);
        let Some(message) = result.content.as_single_text() else {
            panic!("error result must carry model-readable text");
        };
        assert!(message.contains("boom"));
    }

    #[tokio::test]
    async fn output_chunks_replay_through_the_sink() {
        let log = OrderLog::new();
        let chunks = vec![
            ToolOutputChunk {
                channel: ToolOutputChannel::Stdout,
                delta: "line 1".to_owned(),
            },
            ToolOutputChunk {
                channel: ToolOutputChannel::Stderr,
                delta: "warn".to_owned(),
            },
        ];
        let tool =
            ScriptedTool::succeed("chatty", json!(null), log).with_output_chunks(chunks.clone());
        let mut registry = ToolRegistry::new();
        registry.register(tool).expect("register scripted tool");
        let snapshot = registry.snapshot();
        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink_received = received.clone();
        let context = ToolContext::new(
            tokio_util::sync::CancellationToken::new(),
            Arc::new(move |chunk| {
                sink_received.lock().expect("lock received").push(chunk);
            }),
        );
        let call = call("chatty");
        let mut batch = Dispatcher::resolve_batch(&snapshot, std::slice::from_ref(&call));
        let result = Dispatcher::execute(&mut batch, 0, context)
            .expect("single-item batch index")
            .await;
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(*received.lock().expect("lock received"), chunks);
    }

    #[tokio::test]
    async fn hanging_tool_pends_until_cancelled() {
        let log = OrderLog::new();
        let tool = ScriptedTool::hanging("slow", log.clone());
        let cancellation = tokio_util::sync::CancellationToken::new();
        let context = ToolContext::new(cancellation.clone(), Arc::new(|_| {}));
        let execute = tool.execute(json!({}), context);
        tokio::pin!(execute);
        // 未取消时挂起：select 中立即就绪的分支获胜。
        tokio::select! {
            biased;
            result = &mut execute => panic!("hanging tool must pend, got {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        assert_eq!(
            log.entries(),
            vec![LogEntry::ToolExecute {
                name: "slow".to_owned(),
            }]
        );
        cancellation.cancel();
        let error = execute.await.expect_err("cancelled hanging tool errors");
        assert!(error.to_string().contains("interrupted"));
        assert_eq!(
            log.entries(),
            vec![
                LogEntry::ToolExecute {
                    name: "slow".to_owned(),
                },
                LogEntry::ToolCleanup {
                    name: "slow".to_owned(),
                },
            ]
        );
    }
}
