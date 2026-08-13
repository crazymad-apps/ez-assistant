//! 一次 Agent 执行的控制面。

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::{ExecutionRecorder, ToolAuthorizer};

/// 一次 Agent 执行的控制面；三个字段全部必传。
///
/// `authorizer` 必传在类型层面杜绝"无授权闸"的隐藏默认：Core 不提供隐式
/// 放行，需要全放行时由调用方显式装配 [`crate::AllowAllAuthorizer`]。与
/// `ExecutionInput` 的语义输入分离，本结构只承载取消、落账与授权。
pub struct ExecutionContext {
    /// 取消信号；模型流、授权等待与工具执行都必须观察。
    pub cancellation: CancellationToken,
    /// 规范对话两阶段落账 SPI；begin 失败阻断后续副作用。
    pub recorder: Arc<dyn ExecutionRecorder>,
    /// 工具授权闸；每个 Tool Call 独立过闸后才允许执行。
    pub authorizer: Arc<dyn ToolAuthorizer>,
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU64, sync::Mutex, time::Duration};

    use agent_tools::{
        AbsolutePath, Dispatcher, ResolvedBatchItemRef, SessionPathResolver, ShellExecTool,
        ShellExecToolConfig, ShellFuture, ShellOutputSink, ShellRequest, ShellTool, ShellToolError,
        ToolRegistry,
    };
    use agent_types::{
        AssistantMessage, FinishReason, MessageId, ModelIdentity, ProviderId, ToolCall, ToolCallId,
        ToolName,
    };

    use super::*;
    use crate::{
        AllowAllAuthorizer, ConversationDelta, ExchangeReceipt, RecordFuture, ToolAuthorization,
        testutil::block_on,
    };

    /// 记录 delta 的最小 Recorder，验证 SPI 可经 trait object 调用。
    struct ListRecorder {
        deltas: Mutex<Vec<ConversationDelta>>,
        pending: Mutex<Option<AssistantMessage>>,
    }

    struct NeverShell;

    impl ShellTool for NeverShell {
        fn exec<'a>(
            &'a self,
            _request: ShellRequest,
            _sink: ShellOutputSink,
            _cancellation: CancellationToken,
        ) -> ShellFuture<'a> {
            Box::pin(std::future::ready(Err(ShellToolError::InvalidInput {
                message: "not executed".to_owned(),
            })))
        }
    }

    fn shell_tool() -> ShellExecTool {
        let workdir = AbsolutePath::new(std::env::temp_dir()).expect("absolute temp directory");
        let config = ShellExecToolConfig::new(
            Duration::from_secs(120),
            Duration::from_secs(600),
            NonZeroU64::new(1024).expect("non-zero"),
        )
        .expect("valid shell config");
        ShellExecTool::new(
            Arc::new(NeverShell),
            SessionPathResolver::new(workdir),
            config,
        )
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

        fn mark_tool_execution_started<'a>(
            &'a self,
            _receipt: &'a ExchangeReceipt,
            _call_id: &'a agent_types::ToolCallId,
        ) -> RecordFuture<'a, ()> {
            Box::pin(std::future::ready(Ok(())))
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

    fn sample_assistant_message() -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new("message_1").expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("deepseek").expect("valid provider id"),
                "deepseek-reasoner",
            ),
            parts: vec![],
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    #[test]
    fn context_wires_cancellation_recorder_and_authorizer() {
        let recorder = Arc::new(ListRecorder {
            deltas: Mutex::new(vec![]),
            pending: Mutex::new(None),
        });
        let context = ExecutionContext {
            cancellation: CancellationToken::new(),
            recorder: recorder.clone(),
            authorizer: Arc::new(AllowAllAuthorizer),
        };
        assert!(!context.cancellation.is_cancelled());

        let receipt = block_on(
            context
                .recorder
                .begin_tool_exchange(sample_assistant_message()),
        )
        .expect("begin succeeds");
        assert_eq!(receipt.as_str(), "exchange_1");
        assert!(recorder.deltas.lock().expect("lock deltas").is_empty());
        assert!(recorder.pending.lock().expect("lock pending").is_some());

        let call = ToolCall {
            id: ToolCallId::new("call_1").expect("valid call id"),
            name: ToolName::new("shell").expect("valid tool name"),
            arguments: serde_json::json!({"command": "pwd"}),
        };
        let mut registry = ToolRegistry::new();
        registry.register(shell_tool()).expect("register shell");
        let batch = Dispatcher::resolve_batch(&registry.snapshot(), std::slice::from_ref(&call));
        let Some(ResolvedBatchItemRef::Valid(invocation)) = batch.get(0) else {
            panic!("shell resolves");
        };
        let authorization = block_on(context.authorizer.authorize(invocation, &batch));
        assert_eq!(authorization, ToolAuthorization::Allow);
    }
}
