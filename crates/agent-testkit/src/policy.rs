//! 用于确定性策略组合测试的脚本化同步策略。

use std::collections::HashMap;

use agent_core::{PolicyEvaluation, ToolAuthorization, ToolPolicy};
use agent_tools::{ResolvedToolBatch, ResolvedToolInvocation};

use crate::{LogEntry, OrderLog};

/// 对已配置工具名返回明确决策，其他工具返回 Continue 的测试策略。
pub struct ScriptedPolicy {
    decisions: HashMap<String, ToolAuthorization>,
    log: OrderLog,
}

impl ScriptedPolicy {
    /// 使用按工具名配置的明确决策创建策略。
    pub fn with_decisions(
        log: OrderLog,
        decisions: impl IntoIterator<Item = (String, ToolAuthorization)>,
    ) -> Self {
        Self {
            decisions: decisions.into_iter().collect(),
            log,
        }
    }
}

impl ToolPolicy for ScriptedPolicy {
    fn evaluate(
        &self,
        invocation: &ResolvedToolInvocation,
        batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        self.log.push(LogEntry::PolicyEvaluate {
            name: invocation.tool_name().as_str().to_owned(),
            batch_size: batch.len(),
        });
        self.decisions
            .get(invocation.tool_name().as_str())
            .cloned()
            .map_or(PolicyEvaluation::Continue, PolicyEvaluation::Decide)
    }
}
