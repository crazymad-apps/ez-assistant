//! 按装配顺序执行、与上层应用模式无关的工具策略组合机制。
//!
//! Core 只理解“继续匹配”或“做出 Allow/Deny 决策”，不知道
//! Plan/Build、Ask/Auto、名单规则或审批 UI。

use std::{marker::PhantomData, sync::Arc};

use agent_tools::{
    FileAuthorizationFacts, GeneralAuthorizationFacts, ResolvedToolBatch, ResolvedToolInvocation,
    ShellAuthorizationFacts, ToolAuthorizationFacts,
};

use crate::{AuthorizationFuture, ToolAuthorization, ToolAuthorizer};

/// 一条策略对当前 resolved invocation 的同步评估结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyEvaluation {
    /// 当前策略已经做出明确授权决策，策略链立即结束。
    Decide(ToolAuthorization),
    /// 当前策略不处理该调用，继续评估下一条策略。
    Continue,
}

/// 可作为 trait object 装配的通用工具策略。
///
/// `evaluate` 不执行 I/O、没有外部副作用，只读取已冻结的调用事实；它会在
/// `authorize` 创建 future 时同步运行，即使调用方随后尚未 poll 或立即取消该 future。
/// 多条策略按向量顺序执行。
pub trait ToolPolicy: Send + Sync {
    /// 使用当前调用和完整 resolved batch 上下文评估一次策略。
    fn evaluate(
        &self,
        invocation: &ResolvedToolInvocation,
        batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation;
}

/// 只处理某一种具体授权事实 `F` 的类型化策略。
///
/// 例如通用工具、文件工具和 Shell 工具可以分别实现自己的事实类型，
/// 不需要共享一份固定参数结构。
pub trait TypedToolPolicy<F>: Send + Sync
where
    F: ToolAuthorizationFacts,
{
    /// 在事实类型匹配后执行具体策略。
    fn evaluate(
        &self,
        facts: &F,
        invocation: &ResolvedToolInvocation,
        batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation;
}

/// 把类型化策略适配为可放入通用策略链的 [`ToolPolicy`]。
///
/// 适配器先从 invocation 中读取 `F`；类型不匹配时返回
/// [`PolicyEvaluation::Continue`]。这只是组合协议，不是业务授权状态。
pub struct TypedPolicyAdapter<F, P> {
    /// 被适配的具体类型化策略。
    policy: P,
    /// 只在类型系统中关联事实类型，运行时不占用存储。
    facts: PhantomData<fn() -> F>,
}

impl<F, P> TypedPolicyAdapter<F, P> {
    /// 包装一条类型化策略。
    pub fn new(policy: P) -> Self {
        Self {
            policy,
            facts: PhantomData,
        }
    }

    /// 消费适配器并取回内部策略。
    pub fn into_inner(self) -> P {
        self.policy
    }
}

impl<F, P> ToolPolicy for TypedPolicyAdapter<F, P>
where
    F: ToolAuthorizationFacts,
    P: TypedToolPolicy<F>,
{
    fn evaluate(
        &self,
        invocation: &ResolvedToolInvocation,
        batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        match invocation.facts::<F>() {
            Some(facts) => self.policy.evaluate(facts, invocation, batch),
            None => PolicyEvaluation::Continue,
        }
    }
}

/// 通用标准工具事实的适配器别名。
pub type GeneralToolPolicyAdapter<P> = TypedPolicyAdapter<GeneralAuthorizationFacts, P>;

/// 文件工具事实的适配器别名；具体名单和路径规则仍由上层策略实现。
pub type FileToolPolicyAdapter<P> = TypedPolicyAdapter<FileAuthorizationFacts, P>;

/// Shell 工具事实的适配器别名；具体命令和工作目录规则仍由上层策略实现。
pub type ShellToolPolicyAdapter<P> = TypedPolicyAdapter<ShellAuthorizationFacts, P>;

/// 按顺序组合多条策略，并在全部 Continue 时进入必传的最终 Authorizer。
pub struct ComposedToolAuthorizer {
    /// 按装配顺序保存的策略；第一个明确决策胜出。
    policies: Vec<Arc<dyn ToolPolicy>>,
    /// 所有策略均 Continue 时调用的最终授权器，不允许缺省。
    final_authorizer: Arc<dyn ToolAuthorizer>,
}

impl ComposedToolAuthorizer {
    /// 创建策略链：第一个明确 Allow/Deny 胜出，只有全部策略都
    /// Continue 时才调用 `final_authorizer`。
    pub fn new(
        policies: Vec<Arc<dyn ToolPolicy>>,
        final_authorizer: Arc<dyn ToolAuthorizer>,
    ) -> Self {
        Self {
            policies,
            final_authorizer,
        }
    }
}

impl ToolAuthorizer for ComposedToolAuthorizer {
    fn authorize<'a>(
        &'a self,
        invocation: &'a ResolvedToolInvocation,
        batch: &'a ResolvedToolBatch,
    ) -> AuthorizationFuture<'a> {
        for policy in &self.policies {
            if let PolicyEvaluation::Decide(decision) = policy.evaluate(invocation, batch) {
                return Box::pin(std::future::ready(decision));
            }
        }
        self.final_authorizer.authorize(invocation, batch)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroU64,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use agent_tools::{
        AbsolutePath, Dispatcher, ResolvedBatchItemRef, SessionPathResolver, ShellExecTool,
        ShellExecToolConfig, ShellFuture, ShellOutputSink, ShellRequest, ShellTool, ShellToolError,
        ToolRegistry,
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

    fn resolved_batch() -> ResolvedToolBatch {
        let mut registry = ToolRegistry::new();
        registry.register(shell_tool()).expect("register shell");
        Dispatcher::resolve_batch(
            &registry.snapshot(),
            &[ToolCall {
                id: ToolCallId::new("call_1").expect("valid call id"),
                name: ToolName::new("shell").expect("valid tool name"),
                arguments: serde_json::json!({"command": "pwd"}),
            }],
        )
    }

    struct StaticPolicy(PolicyEvaluation);

    impl ToolPolicy for StaticPolicy {
        fn evaluate(
            &self,
            _invocation: &ResolvedToolInvocation,
            _batch: &ResolvedToolBatch,
        ) -> PolicyEvaluation {
            self.0.clone()
        }
    }

    struct CountingAuthorizer(Arc<AtomicUsize>);

    impl ToolAuthorizer for CountingAuthorizer {
        fn authorize<'a>(
            &'a self,
            _invocation: &'a ResolvedToolInvocation,
            _batch: &'a ResolvedToolBatch,
        ) -> AuthorizationFuture<'a> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(ToolAuthorization::Allow))
        }
    }

    struct ShellPolicy(Arc<AtomicUsize>);

    impl TypedToolPolicy<ShellAuthorizationFacts> for ShellPolicy {
        fn evaluate(
            &self,
            _facts: &ShellAuthorizationFacts,
            _invocation: &ResolvedToolInvocation,
            _batch: &ResolvedToolBatch,
        ) -> PolicyEvaluation {
            self.0.fetch_add(1, Ordering::SeqCst);
            PolicyEvaluation::Decide(ToolAuthorization::Allow)
        }
    }

    struct WrongFactsPolicy(Arc<AtomicUsize>);

    impl TypedToolPolicy<String> for WrongFactsPolicy {
        fn evaluate(
            &self,
            _facts: &String,
            _invocation: &ResolvedToolInvocation,
            _batch: &ResolvedToolBatch,
        ) -> PolicyEvaluation {
            self.0.fetch_add(1, Ordering::SeqCst);
            PolicyEvaluation::Decide(ToolAuthorization::Deny {
                reason: "wrong facts must not run".to_owned(),
            })
        }
    }

    fn invocation(batch: &ResolvedToolBatch) -> &ResolvedToolInvocation {
        let Some(ResolvedBatchItemRef::Valid(invocation)) = batch.get(0) else {
            panic!("shell resolves");
        };
        invocation
    }

    #[test]
    fn first_explicit_decision_wins_and_skips_final_authorizer() {
        let final_calls = Arc::new(AtomicUsize::new(0));
        let authorizer = ComposedToolAuthorizer::new(
            vec![
                Arc::new(StaticPolicy(PolicyEvaluation::Continue)),
                Arc::new(StaticPolicy(PolicyEvaluation::Decide(
                    ToolAuthorization::Deny {
                        reason: "mode denied".to_owned(),
                    },
                ))),
                Arc::new(StaticPolicy(PolicyEvaluation::Decide(
                    ToolAuthorization::Allow,
                ))),
            ],
            Arc::new(CountingAuthorizer(final_calls.clone())),
        );
        let batch = resolved_batch();
        assert_eq!(
            block_on(authorizer.authorize(invocation(&batch), &batch)),
            ToolAuthorization::Deny {
                reason: "mode denied".to_owned(),
            }
        );
        assert_eq!(final_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn all_continue_delegates_to_the_mandatory_final_authorizer() {
        let final_calls = Arc::new(AtomicUsize::new(0));
        let authorizer = ComposedToolAuthorizer::new(
            vec![Arc::new(StaticPolicy(PolicyEvaluation::Continue))],
            Arc::new(CountingAuthorizer(final_calls.clone())),
        );
        let batch = resolved_batch();
        assert_eq!(
            block_on(authorizer.authorize(invocation(&batch), &batch)),
            ToolAuthorization::Allow
        );
        assert_eq!(final_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn typed_adapter_continues_on_mismatch_and_calls_matching_policy() {
        let wrong_calls = Arc::new(AtomicUsize::new(0));
        let shell_calls = Arc::new(AtomicUsize::new(0));
        let final_calls = Arc::new(AtomicUsize::new(0));
        let authorizer = ComposedToolAuthorizer::new(
            vec![
                Arc::new(TypedPolicyAdapter::<String, _>::new(WrongFactsPolicy(
                    wrong_calls.clone(),
                ))),
                Arc::new(ShellToolPolicyAdapter::new(ShellPolicy(
                    shell_calls.clone(),
                ))),
            ],
            Arc::new(CountingAuthorizer(final_calls.clone())),
        );
        let batch = resolved_batch();
        assert_eq!(
            block_on(authorizer.authorize(invocation(&batch), &batch)),
            ToolAuthorization::Allow
        );
        assert_eq!(wrong_calls.load(Ordering::SeqCst), 0);
        assert_eq!(shell_calls.load(Ordering::SeqCst), 1);
        assert_eq!(final_calls.load(Ordering::SeqCst), 0);
    }
}
