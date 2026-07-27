//! 工具授权 SPI。
//!
//! 每个 Tool Call 执行前独立过闸：[`ToolAuthorizer::authorize`] 携本轮批次
//! 上下文（同轮全部 Tool Call），Core 只保证逐 call 独立决策；审批编排
//! （串行询问、攒批询问、规则自动放行）由 Runtime 实现借批次上下文自行完成。

use std::{future::Future, pin::Pin};

use agent_types::ToolCall;

/// 一次授权决策的 Future。
pub type AuthorizationFuture<'a> = Pin<Box<dyn Future<Output = ToolAuthorization> + Send + 'a>>;

/// 工具调用的授权闸；实现归 Runtime。
///
/// 对象安全，沿用 `ModelService` 的手写 boxed-future 模式（无 async-trait）。
/// 授权等待必须与取消 race：实现方应观察执行取消信号，避免挂起的审批阻塞
/// 取消收敛；审批交互本身由 Runtime 侧代理完成，`Ask` 不进 Core 词汇表。
pub trait ToolAuthorizer: Send + Sync {
    /// 对单个 Tool Call 做出决策；`batch` 为本轮全部 Tool Call（含 `call` 自身）。
    fn authorize<'a>(
        &'a self,
        call: &'a ToolCall,
        batch: &'a [ToolCall],
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
        _call: &'a ToolCall,
        _batch: &'a [ToolCall],
    ) -> AuthorizationFuture<'a> {
        Box::pin(std::future::ready(ToolAuthorization::Allow))
    }
}

#[cfg(test)]
mod tests {
    use agent_types::{ToolCallId, ToolName};

    use super::*;
    use crate::testutil::block_on;

    fn sample_call(name: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new("call_1").expect("valid call id"),
            name: ToolName::new(name).expect("valid tool name"),
            arguments: serde_json::json!({}),
        }
    }

    #[test]
    fn allow_all_authorizer_allows_every_call() {
        let authorizer = AllowAllAuthorizer;
        let batch = vec![sample_call("read_file"), sample_call("write_file")];
        for call in &batch {
            let authorization = block_on(authorizer.authorize(call, &batch));
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
