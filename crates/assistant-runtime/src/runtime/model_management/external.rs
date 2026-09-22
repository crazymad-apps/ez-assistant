//! 受信宿主发布外部模型的短提交门；与执行最终接纳使用同一模型门。

use assistant_protocol::ModelSettings;
use tokio::sync::{RwLockReadGuard, RwLockWriteGuard};

use super::*;
use crate::{ExternalModelConfiguration, ModelSource};

/// 宿主先获取此门，再短持凭据锁核验当前登录并同步提交。
/// 所有状态变更仅发生在内存；释放凭据锁后调用 finish 发布通知，期间禁止 I/O。
pub struct ExternalModelPublication<'a> {
    runtime: &'a AssistantRuntime,
    operation: RwLockReadGuard<'a, ()>,
    binding: RwLockWriteGuard<'a, ()>,
    changed: bool,
}

impl AssistantRuntime {
    /// 获取外部配置提交门；调用方不得持凭据锁等待此方法。
    ///
    /// # Errors
    /// Runtime 已关闭或模型来源不是 External 时拒绝。
    pub async fn external_model_publication(&self) -> RuntimeResult<ExternalModelPublication<'_>> {
        let operation = self.operation_gate.read().await;
        let binding = self.model_binding_gate.write().await;
        self.ensure_running()?;
        if self.managed_models()?.source != ModelSource::External {
            return Err(invalid("当前模型来源不接受外部配置。"));
        }
        Ok(ExternalModelPublication {
            runtime: self,
            operation,
            binding,
            changed: false,
        })
    }
}

impl ExternalModelPublication<'_> {
    /// 发布已校验的权威获取结果；None 表示权威不可用，必须清除旧配置。
    /// 不读取持久化，不联网，不自行核验宿主身份；以传入的 Arc 标识本次配置归属。
    ///
    /// # Errors
    /// 内存状态锁不可用时返回错误。
    pub fn publish(
        &mut self,
        configuration: Option<Arc<ExternalModelConfiguration>>,
        unavailable_reason: &str,
        fetched_at_ms: i64,
    ) -> RuntimeResult<()> {
        let mut state = self.runtime.managed_models_mut()?;
        state.providers.clear();
        if let Some(configuration) = &configuration {
            let provider = configuration.provider();
            state
                .providers
                .insert(provider.provider_instance_id.clone(), provider);
        }
        state.settings = ModelSettings {
            default_model: configuration
                .as_ref()
                .map(|value| value.selection().clone()),
            vision_model: None,
            management: Some(assistant_protocol::ModelManagementStatus {
                read_only: true,
                unavailable_reason: configuration
                    .is_none()
                    .then(|| unavailable_reason.to_owned()),
                last_success_at_ms: Some(fetched_at_ms),
                last_refresh_error: None,
            }),
        };
        state.external = configuration;
        self.changed = true;
        Ok(())
    }

    /// 只使本次请求捕获的配置失效，旧请求不能清除后来的有效配置。
    /// 返回 false 时宿主不应再次发起因该旧请求触发的刷新。
    ///
    /// # Errors
    /// 内存状态锁不可用时返回错误。
    pub fn invalidate(
        &mut self,
        captured: &Arc<ExternalModelConfiguration>,
        reason: &str,
    ) -> RuntimeResult<bool> {
        let mut state = self.runtime.managed_models_mut()?;
        if !state
            .external
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, captured))
        {
            return Ok(false);
        }
        state.external = None;
        state.providers.clear();
        state.settings.default_model = None;
        if let Some(projection) = &mut state.settings.management {
            projection.unavailable_reason = Some(reason.to_owned());
        }
        self.changed = true;
        Ok(true)
    }

    /// 记录非权威刷新失败，保留已有配置；先前明确失效的配置不会因此恢复。
    ///
    /// # Errors
    /// 内存状态锁不可用时返回错误。
    pub fn refresh_failed(&mut self, reason: &str) -> RuntimeResult<()> {
        let mut state = self.runtime.managed_models_mut()?;
        let unavailable = state.external.is_none();
        if let Some(projection) = &mut state.settings.management {
            projection.last_refresh_error = Some(reason.to_owned());
            if unavailable {
                projection.unavailable_reason = Some("模型配置不可用，请刷新后重试。".to_owned());
            }
        }
        self.changed = true;
        Ok(())
    }

    /// 宿主已释放凭据锁后调用；先释放提交门，再发送可由快照恢复的配置通知。
    pub fn finish(self) {
        let Self {
            runtime,
            operation,
            binding,
            changed,
        } = self;
        drop(binding);
        drop(operation);
        if changed {
            runtime.publish(RuntimeEvent::ConfigChanged);
        }
    }
}
