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
    /// 规范对话落账 SPI；record 失败阻断后续副作用。
    pub recorder: Arc<dyn ExecutionRecorder>,
    /// 工具授权闸；每个 Tool Call 独立过闸后才允许执行。
    pub authorizer: Arc<dyn ToolAuthorizer>,
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use agent_types::{
        AssistantMessage, FinishReason, MessageId, ModelIdentity, ProviderId, ToolCall, ToolCallId,
        ToolName,
    };

    use super::*;
    use crate::{
        AllowAllAuthorizer, ConversationDelta, RecordFuture, RecordReceipt, ToolAuthorization,
        testutil::block_on,
    };

    /// 记录 delta 的最小 Recorder，验证 SPI 可经 trait object 调用。
    struct ListRecorder {
        deltas: Mutex<Vec<ConversationDelta>>,
    }

    impl ExecutionRecorder for ListRecorder {
        fn record<'a>(&'a self, delta: ConversationDelta) -> RecordFuture<'a> {
            Box::pin(async move {
                self.deltas.lock().expect("lock deltas").push(delta);
                Ok(RecordReceipt)
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
                .record(ConversationDelta::Assistant(sample_assistant_message())),
        )
        .expect("record succeeds");
        assert_eq!(receipt, RecordReceipt);
        assert_eq!(recorder.deltas.lock().expect("lock deltas").len(), 1);

        let call = ToolCall {
            id: ToolCallId::new("call_1").expect("valid call id"),
            name: ToolName::new("read_file").expect("valid tool name"),
            arguments: serde_json::json!({}),
        };
        let authorization = block_on(
            context
                .authorizer
                .authorize(&call, std::slice::from_ref(&call)),
        );
        assert_eq!(authorization, ToolAuthorization::Allow);
    }
}
