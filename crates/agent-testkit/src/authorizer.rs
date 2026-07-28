//! 脚本化的 [`ToolAuthorizer`]：按工具名决策，可挂起等待测试放行。
//!
//! 每次授权写入共享 [`OrderLog`](crate::OrderLog)（含批次大小）。挂起模式用
//! [`AuthorizeGate`] 的 Notify/令牌语义同步（禁止 sleep）：`authorize` 进入时
//! 通知 `wait_entered`，随后挂起直到 `release` 或所在 future 被引擎取消丢弃。

use std::{collections::HashMap, sync::Arc};

use agent_core::{AuthorizationFuture, ToolAuthorization, ToolAuthorizer};
use agent_types::ToolCall;
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
}

impl ScriptedAuthorizer {
    /// 全放行授权闸（引擎行为矩阵的默认装配）。
    pub fn allow_all(log: OrderLog) -> Self {
        Self {
            decisions: HashMap::new(),
            gate: None,
            log,
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
        }
    }

    /// 叠加挂起模式：每次授权先经闸门挂起，放行后再按脚本决策。
    pub fn with_gate(mut self, gate: AuthorizeGate) -> Self {
        self.gate = Some(gate);
        self
    }
}

impl ToolAuthorizer for ScriptedAuthorizer {
    fn authorize<'a>(
        &'a self,
        call: &'a ToolCall,
        batch: &'a [ToolCall],
    ) -> AuthorizationFuture<'a> {
        Box::pin(async move {
            self.log.push(LogEntry::Authorize {
                name: call.name.as_str().to_owned(),
                batch_size: batch.len(),
            });
            if let Some(gate) = &self.gate {
                gate.hang_until_released().await;
            }
            self.decisions
                .get(call.name.as_str())
                .cloned()
                .unwrap_or(ToolAuthorization::Allow)
        })
    }
}

#[cfg(test)]
mod tests {
    use agent_types::{ToolCallId, ToolName};

    use super::*;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new("call_1").expect("valid call id"),
            name: ToolName::new(name).expect("valid tool name"),
            arguments: serde_json::json!({}),
        }
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
        let batch = vec![call("read_file"), call("write_file")];
        assert_eq!(
            authorizer.authorize(&batch[0], &batch).await,
            ToolAuthorization::Allow
        );
        assert_eq!(
            authorizer.authorize(&batch[1], &batch).await,
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
        let authorizer = ScriptedAuthorizer::allow_all(log).with_gate(gate.clone());
        let call = call("read_file");
        let pending = authorizer.authorize(&call, std::slice::from_ref(&call));
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
            authorizer
                .authorize(&call, std::slice::from_ref(&call))
                .await,
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
