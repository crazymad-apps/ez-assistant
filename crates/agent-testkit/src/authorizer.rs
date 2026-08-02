//! 脚本化的 [`ToolAuthorizer`]：按工具名决策，可挂起等待测试放行。
//!
//! 每次授权写入共享 [`OrderLog`](crate::OrderLog)（含批次大小）。挂起模式用
//! [`AuthorizeGate`] 的 Notify/令牌语义同步（禁止 sleep）：`authorize` 进入时
//! 通知 `wait_entered`，随后挂起直到 `release` 或所在 future 被引擎取消丢弃。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use agent_core::{AuthorizationFuture, ToolAuthorization, ToolAuthorizer};
use agent_tools::{ResolvedToolBatch, ResolvedToolInvocation};
use agent_types::{ToolCallId, ToolName};
use serde_json::Value;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::OrderLog;
use crate::order::LogEntry;

/// 授权挂起闸门：连接测试与被测执行，全程 gate/Notify 语义，无 sleep。
///
/// - 被测侧（authorizer）：进入 `authorize` 时 `notify_one` 通知进入，随后挂起
///   直到放行（或 future 被引擎的取消 race 丢弃——挂起本身可安全丢弃）；
/// - 测试侧：[`wait_entered`](Self::wait_entered) 等到第一次进入后触发取消等
///   动作，[`release`](Self::release) 放行所有挂起与后续的授权。
#[derive(Clone, Default)]
pub struct AuthorizeGate {
    entered: Arc<Notify>,
    released: CancellationToken,
}

impl AuthorizeGate {
    /// 创建未放行的闸门。
    pub fn new() -> Self {
        Self::default()
    }

    /// 等待第一个 `authorize` 进入；已发生时立即返回。
    pub async fn wait_entered(&self) {
        self.entered.notified().await;
    }

    /// 放行：当前挂起与后续的授权全部立即通过；幂等。
    pub fn release(&self) {
        self.released.cancel();
    }

    /// 通知进入并挂起直到放行（authorizer 侧）。
    async fn hang_until_released(&self) {
        self.entered.notify_one();
        self.released.cancelled().await;
    }
}

/// 脚本化的授权闸 Fake；未配置决策的工具名默认 `Allow`。
pub struct ScriptedAuthorizer {
    decisions: HashMap<String, ToolAuthorization>,
    gate: Option<AuthorizeGate>,
    log: OrderLog,
    observations: Arc<Mutex<Vec<AuthorizationObservation>>>,
}

/// 脚本化 Authorizer 观察到的一次 resolved 授权请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationObservation {
    /// 模型协议中的原始 Tool Call ID。
    pub call_id: ToolCallId,
    /// 冻结后的模型可见工具名。
    pub tool_name: ToolName,
    /// 授权代码实际看到的完整 resolved 参数。
    pub resolved_arguments: Value,
    /// 原 resolved batch 的位置数，包含 invalid 位置。
    pub batch_size: usize,
}

impl ScriptedAuthorizer {
    /// 全放行授权闸（引擎行为矩阵的默认装配）。
    pub fn allow_all(log: OrderLog) -> Self {
        Self {
            decisions: HashMap::new(),
            gate: None,
            log,
            observations: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 按工具名配置决策；未出现的名字默认 `Allow`。
    pub fn with_decisions(
        log: OrderLog,
        decisions: impl IntoIterator<Item = (String, ToolAuthorization)>,
    ) -> Self {
        Self {
            decisions: decisions.into_iter().collect(),
            gate: None,
            log,
            observations: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 叠加挂起模式：每次授权先经闸门挂起，放行后再按脚本决策。
    pub fn with_gate(mut self, gate: AuthorizeGate) -> Self {
        self.gate = Some(gate);
        self
    }

    /// 按调用顺序返回 Authorizer 观察到的 resolved 请求快照。
    pub fn observations(&self) -> Vec<AuthorizationObservation> {
        self.observations
            .lock()
            .expect("authorization observations mutex poisoned")
            .clone()
    }
}

impl ToolAuthorizer for ScriptedAuthorizer {
    fn authorize<'a>(
        &'a self,
        invocation: &'a ResolvedToolInvocation,
        batch: &'a ResolvedToolBatch,
    ) -> AuthorizationFuture<'a> {
        Box::pin(async move {
            self.log.push(LogEntry::Authorize {
                name: invocation.tool_name().as_str().to_owned(),
                batch_size: batch.len(),
            });
            self.observations
                .lock()
                .expect("authorization observations mutex poisoned")
                .push(AuthorizationObservation {
                    call_id: invocation.call_id().clone(),
                    tool_name: invocation.tool_name().clone(),
                    resolved_arguments: invocation.resolved_arguments().clone(),
                    batch_size: batch.len(),
                });
            if let Some(gate) = &self.gate {
                gate.hang_until_released().await;
            }
            self.decisions
                .get(invocation.tool_name().as_str())
                .cloned()
                .unwrap_or(ToolAuthorization::Allow)
        })
    }
}

#[cfg(test)]
mod tests {
    use agent_tools::{Dispatcher, ResolvedBatchItemRef, ToolRegistry};
    use agent_types::ToolCall;
    use serde_json::json;

    use super::*;
    use crate::ScriptedTool;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new("call_1").expect("valid call id"),
            name: ToolName::new(name).expect("valid tool name"),
            arguments: serde_json::json!({}),
        }
    }

    fn resolved(calls: &[ToolCall], log: &OrderLog) -> agent_tools::ResolvedToolBatch {
        let mut registry = ToolRegistry::new();
        for call in calls {
            registry
                .register(ScriptedTool::succeed(
                    call.name.as_str(),
                    json!(null),
                    log.clone(),
                ))
                .expect("register scripted tool");
        }
        Dispatcher::resolve_batch(&registry.snapshot(), calls)
    }

    #[tokio::test]
    async fn decisions_are_scripted_by_name_with_allow_default() {
        let log = OrderLog::new();
        let authorizer = ScriptedAuthorizer::with_decisions(
            log.clone(),
            [(
                "write_file".to_owned(),
                ToolAuthorization::Deny {
                    reason: "no writes".to_owned(),
                },
            )],
        );
        let calls = vec![call("read_file"), call("write_file")];
        let batch = resolved(&calls, &log);
        let Some(ResolvedBatchItemRef::Valid(read)) = batch.get(0) else {
            panic!("read resolves");
        };
        let Some(ResolvedBatchItemRef::Valid(write)) = batch.get(1) else {
            panic!("write resolves");
        };
        assert_eq!(
            authorizer.authorize(read, &batch).await,
            ToolAuthorization::Allow
        );
        assert_eq!(
            authorizer.authorize(write, &batch).await,
            ToolAuthorization::Deny {
                reason: "no writes".to_owned(),
            }
        );
        // 日志含批次大小（同轮全部 Tool Call 数）。
        assert_eq!(
            log.entries(),
            vec![
                LogEntry::Authorize {
                    name: "read_file".to_owned(),
                    batch_size: 2,
                },
                LogEntry::Authorize {
                    name: "write_file".to_owned(),
                    batch_size: 2,
                },
            ]
        );
    }

    #[tokio::test]
    async fn gate_hangs_authorize_until_released() {
        let log = OrderLog::new();
        let gate = AuthorizeGate::new();
        let authorizer = ScriptedAuthorizer::allow_all(log.clone()).with_gate(gate.clone());
        let calls = [call("read_file")];
        let batch = resolved(&calls, &log);
        let Some(ResolvedBatchItemRef::Valid(invocation)) = batch.get(0) else {
            panic!("read resolves");
        };
        let pending = authorizer.authorize(invocation, &batch);
        tokio::pin!(pending);

        // 未放行时挂起：先驱动一次 poll（authorize 进入并发出 entered 通知后挂起）。
        tokio::select! {
            biased;
            decision = &mut pending => panic!("authorize must hang, got {decision:?}"),
            () = tokio::task::yield_now() => {}
        }
        gate.wait_entered().await;

        gate.release();
        assert_eq!(pending.await, ToolAuthorization::Allow);
        // 放行后后续授权不再挂起。
        assert_eq!(
            authorizer.authorize(invocation, &batch).await,
            ToolAuthorization::Allow
        );
    }

    #[tokio::test]
    async fn entered_before_wait_still_unblocks_the_test() {
        // notify_one 的许可存储：先进入后等待也能立即返回。
        let gate = AuthorizeGate::new();
        gate.entered.notify_one();
        gate.wait_entered().await;
    }
}
