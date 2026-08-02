//! 工具授权 SPI。
//!
//! 每个 resolved invocation 执行前独立过闸：[`ToolAuthorizer::authorize`]
//! 携本轮完整 resolved batch 上下文，Core 只保证逐调用独立决策；审批编排
//! （串行询问、攒批询问、规则自动放行）由 Runtime 实现借批次上下文自行完成。

use std::{future::Future, pin::Pin};

use agent_tools::{ResolvedToolBatch, ResolvedToolInvocation};

/// 一次授权决策的 Future。
pub type AuthorizationFuture<'a> = Pin<Box<dyn Future<Output = ToolAuthorization> + Send + 'a>>;

/// 工具调用的授权闸；实现归 Runtime。
///
/// 对象安全，沿用 `ModelService` 的手写 boxed-future 模式（无 async-trait）。
/// 授权等待必须与取消 race：实现方应观察执行取消信号，避免挂起的审批阻塞
/// 取消收敛；审批交互本身由 Runtime 侧代理完成，`Ask` 不进 Core 词汇表。
pub trait ToolAuthorizer: Send + Sync {
    /// 对单个 resolved invocation 做出决策；`batch` 保留本轮原数量与顺序。
    fn authorize<'a>(
        &'a self,
        invocation: &'a ResolvedToolInvocation,
        batch: &'a ResolvedToolBatch,
    ) -> AuthorizationFuture<'a>;
}

/// 授权决策；拒绝不是执行错误。
///
/// `Deny` 在授权闸处转换为错误 `ToolResult` 回喂模型、循环继续——对模型与循环
/// 不存在"被拒绝"类别；`reason` 是模型唯一可见信息，措辞归 Runtime。
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ToolAuthorization {
    /// 允许执行。
    Allow,
    /// 拒绝执行；对应工具不执行，`reason` 转为错误 `ToolResult` 的模型可读内容。
    Deny {
        /// 模型可读的拒绝原因；措辞归 Runtime。
        reason: String,
    },
}

/// 显式装配用的全放行授权闸；Core 不提供任何隐式默认。
pub struct AllowAllAuthorizer;

impl ToolAuthorizer for AllowAllAuthorizer {
    fn authorize<'a>(
        &'a self,
        _invocation: &'a ResolvedToolInvocation,
        _batch: &'a ResolvedToolBatch,
    ) -> AuthorizationFuture<'a> {
        Box::pin(std::future::ready(ToolAuthorization::Allow))
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU64, sync::Arc, time::Duration};

    use agent_tools::{
        AbsolutePath, Dispatcher, SessionPathResolver, ShellExecTool, ShellExecToolConfig,
        ShellFuture, ShellOutputSink, ShellRequest, ShellTool, ShellToolError, ToolRegistry,
    };
    use agent_types::{ToolCall, ToolCallId, ToolName};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::testutil::block_on;

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
        let config = ShellExecToolConfig::new(
            Duration::from_secs(120),
            Duration::from_secs(600),
            NonZeroU64::new(1024).expect("non-zero"),
        )
        .expect("valid shell config");
        ShellExecTool::new(
            Arc::new(NeverShell),
            SessionPathResolver::new(
                AbsolutePath::new(std::env::temp_dir()).expect("absolute temp directory"),
            ),
            config,
        )
    }

    fn sample_call(command: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new("call_1").expect("valid call id"),
            name: ToolName::new("shell").expect("valid tool name"),
            arguments: serde_json::json!({"command": command}),
        }
    }

    fn sample_batch(calls: &[ToolCall]) -> agent_tools::ResolvedToolBatch {
        let mut registry = ToolRegistry::new();
        registry.register(shell_tool()).expect("register shell");
        Dispatcher::resolve_batch(&registry.snapshot(), calls)
    }

    #[test]
    fn allow_all_authorizer_allows_every_call() {
        let authorizer = AllowAllAuthorizer;
        let calls = vec![sample_call("pwd"), sample_call("ls")];
        let batch = sample_batch(&calls);
        for item in batch.iter() {
            let agent_tools::ResolvedBatchItemRef::Valid(invocation) = item else {
                panic!("shell resolves");
            };
            let authorization = block_on(authorizer.authorize(invocation, &batch));
            assert_eq!(authorization, ToolAuthorization::Allow);
        }
    }

    #[test]
    fn authorization_round_trips_serde() {
        let decisions = vec![
            ToolAuthorization::Allow,
            ToolAuthorization::Deny {
                reason: "user rejected the command".to_owned(),
            },
        ];
        for decision in decisions {
            let json = serde_json::to_string(&decision).expect("serialize decision");
            assert_eq!(
                serde_json::from_str::<ToolAuthorization>(&json).expect("deserialize decision"),
                decision
            );
        }
        // 稳定 tag：allow/deny 蛇形命名。
        let json = serde_json::to_value(ToolAuthorization::Deny {
            reason: "no".to_owned(),
        })
        .expect("serialize decision to value");
        assert_eq!(json["decision"], "deny");
        assert_eq!(json["reason"], "no");
    }
}
