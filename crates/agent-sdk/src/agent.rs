use std::sync::Arc;

use agent_core::{
    AgentExecution, ExecutionBudget, ExecutionContext, ExecutionInput, ExecutionSpec,
    ToolAuthorizer,
};
use agent_model::SystemPromptSnapshot;
use agent_types::ToolDefinition;
use tokio_util::sync::CancellationToken;

use crate::ephemeral_recorder::EphemeralExecutionRecorder;

/// 一个持有冻结执行配置的 Agent Facade。
///
/// Agent 不保存动态 Conversation，也不实现 Clone。调用方负责同一会话的执行串行，
/// 不同 Agent 可以共享线程安全的底层模型服务。
pub struct Agent {
    spec: ExecutionSpec,
}

impl Agent {
    pub(crate) fn from_spec(spec: ExecutionSpec) -> Self {
        Self { spec }
    }

    /// 使用调用方显式提供的 Recorder、Authorizer 与取消令牌启动一次执行。
    ///
    /// 该路径适合拥有权威 Journal 的 Runtime 风格宿主。
    ///
    /// # Panics
    ///
    /// 当前线程不在可用于 `tokio::spawn` 的 Tokio Runtime 中时会 panic。
    pub fn start(&self, input: ExecutionInput, context: ExecutionContext) -> AgentExecution {
        AgentExecution::start(self.spec.clone(), input, context)
    }

    /// 使用调用方给出的“本业务执行剩余预算”启动 continuation。
    ///
    /// 只覆盖本次派生 spec 的预算，冻结模型、Prompt、工具、Guardrail 和请求配置保持不变。
    pub fn start_with_budget(
        &self,
        input: ExecutionInput,
        context: ExecutionContext,
        budget: ExecutionBudget,
    ) -> AgentExecution {
        let mut spec = self.spec.clone();
        spec.budget = budget;
        AgentExecution::start(spec, input, context)
    }

    /// 使用一次执行独享的不可恢复 Recorder 启动一次执行。
    ///
    /// 临时 Recorder 仍严格执行 tool exchange 的 begin/complete 协议，但完成后立即
    /// 丢弃消息，不提供读取、持久化或进程异常恢复能力。
    ///
    /// # Panics
    ///
    /// 当前线程不在可用于 `tokio::spawn` 的 Tokio Runtime 中时会 panic。
    pub fn start_ephemeral(
        &self,
        input: ExecutionInput,
        cancellation: CancellationToken,
        authorizer: Arc<dyn ToolAuthorizer>,
    ) -> AgentExecution {
        self.start(
            input,
            ExecutionContext {
                cancellation,
                recorder: Arc::new(EphemeralExecutionRecorder::new()),
                authorizer,
            },
        )
    }

    /// 只读访问构建 Agent 时冻结的完整 System Prompt。
    pub fn system_prompt(&self) -> &SystemPromptSnapshot {
        &self.spec.system_prompt
    }

    /// 按注册顺序只读访问模型可见的冻结工具定义。
    pub fn tool_definitions(&self) -> &[ToolDefinition] {
        self.spec.tools.definitions()
    }

    /// 只读访问冻结的完整业务执行预算。
    pub fn execution_budget(&self) -> &ExecutionBudget {
        &self.spec.budget
    }
}
