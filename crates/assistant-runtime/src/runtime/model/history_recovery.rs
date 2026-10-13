//! 单次逻辑模型调用的服务端历史恢复；不持有 Run 或 child 级恢复额度。
use agent_model::{
    ModelAttemptObserver, ModelCallContext, ModelCapabilities, ModelError, ModelRequest,
    ModelRetryPolicy, ModelService, ModelStreamFuture, RetryingModelService,
};
use std::sync::Arc;

/// 每次 stream 创建独立恢复额度，外层有限重试的所有 attempt 共用该额度。
pub(super) struct CallScopedHistoryModelService {
    inner: Arc<dyn ModelService>,
    policy: Option<ModelRetryPolicy>,
    observer: Option<Arc<dyn ModelAttemptObserver>>,
}
impl CallScopedHistoryModelService {
    pub(super) fn new(
        inner: Arc<dyn ModelService>,
        policy: Option<ModelRetryPolicy>,
        observer: Option<Arc<dyn ModelAttemptObserver>>,
    ) -> Self {
        Self {
            inner,
            policy,
            observer,
        }
    }
}
impl ModelService for CallScopedHistoryModelService {
    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }
    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }
    fn max_input_tokens(&self) -> Option<u64> {
        self.inner.max_input_tokens()
    }

    fn context_policy(&self) -> &agent_model::ContextPolicy {
        self.inner.context_policy()
    }

    fn count_input_tokens<'a>(
        &'a self,
        request: &'a agent_model::ModelRequest,
        context: &'a mut agent_model::ModelCallContext,
    ) -> agent_model::ModelInputTokenCountFuture<'a> {
        self.inner.count_input_tokens(request, context)
    }

    fn compact(
        &self,
        request: agent_model::ModelRequest,
        context: agent_model::ModelCallContext,
    ) -> agent_model::ModelCompactionFuture<'_> {
        self.inner.compact(request, context)
    }

    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        Box::pin(async move {
            let recovering: Arc<dyn ModelService> = Arc::new(RecoveringHistoryModelService::new(
                self.inner.clone(),
                Default::default(),
            ));
            let model: Arc<dyn ModelService> = match (&self.policy, &self.observer) {
                (Some(policy), Some(observer)) => Arc::new(RetryingModelService::with_observer(
                    recovering,
                    policy.clone(),
                    observer.clone(),
                )),
                (Some(policy), None) => {
                    Arc::new(RetryingModelService::new(recovering, policy.clone()))
                }
                (None, Some(observer)) => Arc::new(super::ObservedModelService {
                    inner: recovering,
                    observer: observer.clone(),
                }),
                (None, None) => recovering,
            };
            model.stream(request, context).await
        })
    }
}

/// 单次逻辑模型调用独享的一次恢复额度。只拦截建流前的明确旧 ID 拒绝，
/// 不改变 Core step/工具预算，不重新执行工具，不观察或重试流内错误。
pub(super) struct RecoveringHistoryModelService {
    inner: Arc<dyn ModelService>,
    rebuilt: Arc<std::sync::atomic::AtomicBool>,
}
impl RecoveringHistoryModelService {
    pub(super) fn new(
        inner: Arc<dyn ModelService>,
        rebuilt: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self { inner, rebuilt }
    }
}
impl ModelService for RecoveringHistoryModelService {
    fn capabilities(&self) -> &agent_model::ModelCapabilities {
        self.inner.capabilities()
    }
    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }
    fn max_input_tokens(&self) -> Option<u64> {
        self.inner.max_input_tokens()
    }

    fn context_policy(&self) -> &agent_model::ContextPolicy {
        self.inner.context_policy()
    }

    fn count_input_tokens<'a>(
        &'a self,
        request: &'a agent_model::ModelRequest,
        context: &'a mut agent_model::ModelCallContext,
    ) -> agent_model::ModelInputTokenCountFuture<'a> {
        self.inner.count_input_tokens(request, context)
    }

    fn compact(
        &self,
        request: agent_model::ModelRequest,
        context: agent_model::ModelCallContext,
    ) -> agent_model::ModelCompactionFuture<'_> {
        self.inner.compact(request, context)
    }

    fn stream(
        &self,
        request: agent_model::ModelRequest,
        mut context: agent_model::ModelCallContext,
    ) -> ModelStreamFuture<'_> {
        Box::pin(async move {
            // 一旦旧链失效，本次逻辑调用的后续 attempt 始终重发完整可靠上下文。
            // 重建本身的瞬态失败交由外层有限预算处理，不能再恢复失效旧 ID。
            context.rebuild_server_history |=
                self.rebuilt.load(std::sync::atomic::Ordering::Acquire);
            match self.inner.stream(request.clone(), context.clone()).await {
                Err(ModelError::HistoryUnavailable)
                    if !self.rebuilt.swap(true, std::sync::atomic::Ordering::AcqRel) =>
                {
                    if context.cancellation.is_cancelled() {
                        return Err(ModelError::Cancelled);
                    }
                    context.rebuild_server_history = true;
                    self.inner.stream(request, context).await
                }
                result => result,
            }
        })
    }
}
