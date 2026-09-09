//! Runtime 单一模型设置快照。网络不持配置写门；短提交后替换对应 Provider Arc 并广播配置变化。

use super::AssistantRuntime;
use crate::{ModelDiscoveryRequest, ModelParameters, RuntimeError, RuntimeResult, StoredProvider};
use assistant_protocol::{
    CreateProviderRequest, DiscoveredModel, GetModelConfigurationRequest,
    ListFixedModelConfigsRequest, ModelConfigOrigin, ModelConfigurationDetail,
    ModelConfigurationSource, ModelFixedConfig, ModelSelection, ModelSettings, ProviderConnection,
    ProviderCredentialChange, ProviderInstanceId, ProviderSummary, RuntimeEvent,
    SaveModelFixedConfigRequest, SecretValue, UpdateProviderRequest,
};
use std::sync::{Arc, RwLockReadGuard, RwLockWriteGuard};

use crate::config::ManagedModels;

enum SelectionPurpose {
    Default,
    AuxiliaryVision,
}

impl AssistantRuntime {
    /// 新选择优先读取已保存模型，无固定记录时由在线目录确认身份；不写 TOML。
    pub async fn set_default_model(
        &self,
        request: assistant_protocol::SetDefaultModelRequest,
    ) -> RuntimeResult<ModelSettings> {
        self.save_model_selection(request.selection, SelectionPurpose::Default)
            .await
    }

    /// 清除辅助选择只修改数据库引用，保留 TOML 中的辅助调用预算。
    pub async fn set_auxiliary_vision_model(
        &self,
        request: assistant_protocol::SetAuxiliaryVisionModelRequest,
    ) -> RuntimeResult<ModelSettings> {
        self.save_model_selection(request.selection, SelectionPurpose::AuxiliaryVision)
            .await
    }

    /// 网络准备不持写门；提交时再次核对 Provider Arc，防止连接或固定值已改变。
    /// 短提交只替换本次用途的引用，保留并发提交的另一用途；落库完成后才更新快照和发事件。
    async fn save_model_selection(
        &self,
        selection: Option<ModelSelection>,
        purpose: SelectionPurpose,
    ) -> RuntimeResult<ModelSettings> {
        self.ensure_running()?;
        let captured = if let Some(selection) = &selection {
            validate_selection(selection)?;
            let snapshot = self.config_registry.snapshot()?;
            let prepared = self
                .config_registry
                .prepare_model(
                    &snapshot,
                    Some(selection),
                    self.store.as_ref(),
                    self.model_factory.as_ref(),
                )
                .await?;
            if matches!(purpose, SelectionPurpose::AuxiliaryVision)
                && !prepared.model.capabilities().image_input
            {
                return Err(invalid("辅助识图模型必须明确支持图片输入。"));
            }
            Some(prepared)
        } else {
            None
        };
        let _operation = self.operation_gate.read().await;
        let _gate = self.model_binding_gate.write().await;
        self.ensure_running()?;
        if let Some(prepared) = captured {
            prepared.ensure_current(&self.config_registry)?;
        }
        let mut settings = self.managed_models()?.settings.clone();
        let target = match purpose {
            SelectionPurpose::Default => &mut settings.default_model,
            SelectionPurpose::AuxiliaryVision => &mut settings.vision_model,
        };
        if *target == selection {
            return Ok(settings);
        }
        *target = selection;
        self.store
            .save_model_settings(settings.clone())
            .await
            .map_err(|e| RuntimeError::from_store("save model selection", e))?;
        self.managed_models_mut()?.settings = settings.clone();
        self.publish(RuntimeEvent::ConfigChanged);
        Ok(settings)
    }

    /// 启动恢复仅加载连接和全局选择，固定参数按二元键读取；不得导入旧 TOML 模型。
    pub async fn restore_model_settings(&self) -> RuntimeResult<()> {
        let _gate = self.model_binding_gate.write().await;
        let providers = self
            .store
            .load_providers()
            .await
            .map_err(|e| RuntimeError::from_store("load providers", e))?;
        let settings = self
            .store
            .load_model_settings()
            .await
            .map_err(|e| RuntimeError::from_store("load model settings", e))?;
        let mut state = self.managed_models_mut()?;
        state.providers = providers
            .into_iter()
            .map(|p| (p.provider_instance_id.clone(), Arc::new(p)))
            .collect();
        state.settings = settings;
        Ok(())
    }
    pub fn list_providers(&self) -> RuntimeResult<Vec<ProviderSummary>> {
        self.ensure_running()?;
        Ok(self
            .managed_models()?
            .providers
            .values()
            .map(|provider| summary(provider))
            .collect())
    }
    pub fn get_model_settings(&self) -> RuntimeResult<ModelSettings> {
        self.ensure_running()?;
        Ok(self.managed_models()?.settings.clone())
    }
    pub async fn create_provider(
        &self,
        request: CreateProviderRequest,
    ) -> RuntimeResult<ProviderSummary> {
        validate_connection(&request.connection)?;
        let api_key = credential(SecretValue::new(String::new()), request.credential)?;
        validate_provider_credential(&request.connection, &api_key)?;
        let id = ProviderInstanceId::new(
            crate::id::generate("provider").map_err(|_| invalid("服务商标识无法创建。"))?,
        )
        .map_err(|_| invalid("服务商标识无法创建。"))?;
        let provider = Arc::new(StoredProvider {
            provider_instance_id: id.clone(),
            connection: request.connection,
            api_key,
        });
        let _operation = self.operation_gate.read().await;
        let _gate = self.model_binding_gate.write().await;
        self.ensure_running()?;
        if self.managed_models()?.providers.contains_key(&id) {
            return Err(invalid("服务商标识冲突，请重试。"));
        }
        self.store
            .put_provider(provider.as_ref().clone())
            .await
            .map_err(|e| RuntimeError::from_store("create provider", e))?;
        self.managed_models_mut()?
            .providers
            .insert(id, provider.clone());
        self.publish(RuntimeEvent::ConfigChanged);
        Ok(summary(&provider))
    }
    pub async fn update_provider(
        &self,
        request: UpdateProviderRequest,
    ) -> RuntimeResult<ProviderSummary> {
        validate_connection(&request.connection)?;
        let _operation = self.operation_gate.read().await;
        let _gate = self.model_binding_gate.write().await;
        self.ensure_running()?;
        let old = self.provider(&request.provider_instance_id)?;
        let api_key = credential(old.api_key.clone(), request.credential)?;
        validate_provider_credential(&request.connection, &api_key)?;
        // 连接与方言变更不删除固定值；所有已有能力必须在新实例规则中仍可表达。
        let mut offset = 0;
        loop {
            let records = self
                .store
                .list_model_fixed_configs(request.provider_instance_id.clone(), offset, 200)
                .await
                .map_err(|e| RuntimeError::from_store("validate provider fixed models", e))?;
            for record in &records {
                validate_provider_parameters(&request.connection, &record.parameters)?;
            }
            if records.len() < 200 {
                break;
            }
            offset += 200;
        }
        let provider = Arc::new(StoredProvider {
            provider_instance_id: request.provider_instance_id.clone(),
            connection: request.connection,
            api_key,
        });
        self.store
            .put_provider(provider.as_ref().clone())
            .await
            .map_err(|e| RuntimeError::from_store("update provider", e))?;
        self.managed_models_mut()?
            .providers
            .insert(request.provider_instance_id, provider.clone());
        self.publish(RuntimeEvent::ConfigChanged);
        Ok(summary(&provider))
    }
    pub async fn delete_provider(
        &self,
        id: ProviderInstanceId,
    ) -> RuntimeResult<assistant_protocol::ProviderUsage> {
        let _operation = self.operation_gate.read().await;
        let _gate = self.model_binding_gate.write().await;
        self.ensure_running()?;
        self.provider(&id)?;
        let usage = self
            .store
            .remove_provider(id.clone())
            .await
            .map_err(|e| RuntimeError::from_store("delete provider", e))?;
        self.managed_models_mut()?.providers.remove(&id);
        self.publish(RuntimeEvent::ConfigChanged);
        Ok(usage)
    }
    /// 删除确认前按存储权威快照查询全量影响；只持短读门，不装配会话或请求模型。
    pub async fn get_provider_usage(
        &self,
        id: ProviderInstanceId,
    ) -> RuntimeResult<assistant_protocol::ProviderUsage> {
        let _gate = self.model_binding_gate.read().await;
        self.ensure_running()?;
        self.provider(&id)?;
        self.store
            .provider_usage(id)
            .await
            .map_err(|error| RuntimeError::from_store("get provider usage", error))
    }
    pub async fn list_provider_models(
        &self,
        id: ProviderInstanceId,
    ) -> RuntimeResult<Vec<DiscoveredModel>> {
        self.ensure_running()?;
        let provider = self.provider(&id)?;
        let mut models = self.discover_provider(&provider).await?;
        for model in &mut models {
            let detail = crate::config::model_templates::configuration_detail(
                &provider.connection,
                ModelSelection {
                    provider_instance_id: id.clone(),
                    model_id: model.model_id.clone(),
                },
                model.metadata.clone(),
            );
            model.configuration = Some(assistant_protocol::ModelConfigurationSummary {
                uses_template: detail
                    .field_sources
                    .values()
                    .any(|source| *source == ModelConfigurationSource::Template),
                requires_configuration: crate::validate_fixed_model_parameters(&detail.parameters)
                    .is_err()
                    || detail.parameters.streaming
                        != assistant_protocol::ModelFeatureSupport::Supported,
            });
        }
        self.ensure_provider_current(&provider)?;
        Ok(models)
    }
    pub async fn get_model_configuration(
        &self,
        request: GetModelConfigurationRequest,
    ) -> RuntimeResult<ModelConfigurationDetail> {
        let selection = request.selection;
        validate_selection(&selection)?;
        self.ensure_running()?;
        let provider = self.provider(&selection.provider_instance_id)?;
        let fixed = self
            .store
            .get_model_fixed_config(selection.clone())
            .await
            .map_err(|e| RuntimeError::from_store("load fixed model", e))?;
        if let Some(fixed) = fixed {
            self.ensure_provider_current(&provider)?;
            return Ok(ModelConfigurationDetail {
                origin: fixed.origin,
                selection,
                parameters: fixed.parameters,
                source: ModelConfigurationSource::Fixed,
                updated_at_ms: Some(fixed.updated_at_ms),
                field_sources: Default::default(),
                template_document: None,
                template_checked_on: None,
            });
        }
        if request.origin == ModelConfigOrigin::Manual {
            self.ensure_provider_current(&provider)?;
            let mut detail = crate::config::model_templates::configuration_detail(
                &provider.connection,
                selection,
                ModelParameters::default(),
            );
            detail.origin = ModelConfigOrigin::Manual;
            return Ok(detail);
        }
        let model = self
            .discover_provider(&provider)
            .await?
            .into_iter()
            .find(|m| m.model_id == selection.model_id)
            .ok_or_else(|| invalid("本次在线列表中没有该模型，请重新选择。"))?;
        self.ensure_provider_current(&provider)?;
        Ok(crate::config::model_templates::configuration_detail(
            &provider.connection,
            selection,
            model.metadata,
        ))
    }
    pub async fn list_fixed_model_configs(
        &self,
        request: ListFixedModelConfigsRequest,
    ) -> RuntimeResult<Vec<ModelFixedConfig>> {
        self.ensure_running()?;
        let provider = self.provider(&request.provider_instance_id)?;
        let records = self
            .store
            .list_model_fixed_configs(
                request.provider_instance_id,
                request.offset,
                request.limit.clamp(1, 200),
            )
            .await
            .map_err(|e| RuntimeError::from_store("list fixed models", e))?;
        self.ensure_provider_current(&provider)?;
        Ok(records)
    }
    pub async fn save_model_fixed_config(
        &self,
        request: SaveModelFixedConfigRequest,
    ) -> RuntimeResult<ModelFixedConfig> {
        validate_selection(&request.selection)?;
        self.ensure_running()?;
        let provider = self.provider(&request.selection.provider_instance_id)?;
        validate_provider_parameters(&provider.connection, &request.parameters)?;
        crate::validate_fixed_model_parameters(&request.parameters)
            .map_err(|_| invalid("固定参数不完整或数值、思考设置组合无效。"))?;
        let existing = self
            .store
            .get_model_fixed_config(request.selection.clone())
            .await
            .map_err(|e| RuntimeError::from_store("load fixed model", e))?;
        if existing
            .as_ref()
            .is_some_and(|fixed| fixed.origin != request.origin)
        {
            return Err(invalid("模型来源不可修改，请重新读取原配置。"));
        }
        if request.origin == ModelConfigOrigin::Online
            && existing.is_none()
            && !self
                .discover_provider(&provider)
                .await?
                .iter()
                .any(|m| m.model_id == request.selection.model_id)
        {
            return Err(invalid("首次固定配置必须选择当前在线列表中的模型。"));
        }
        let _operation = self.operation_gate.read().await;
        let _gate = self.model_binding_gate.write().await;
        self.ensure_running()?;
        self.ensure_provider_current(&provider)?;
        let fixed = ModelFixedConfig {
            origin: request.origin,
            selection: request.selection,
            parameters: request.parameters,
            updated_at_ms: super::now_ms()?,
        };
        self.store
            .put_model_fixed_config(fixed.clone())
            .await
            .map_err(|e| RuntimeError::from_store("save fixed model", e))?;
        self.publish_provider_configuration(provider)?;
        Ok(fixed)
    }
    pub async fn reset_model_fixed_config(&self, selection: ModelSelection) -> RuntimeResult<()> {
        validate_selection(&selection)?;
        let _operation = self.operation_gate.read().await;
        let _gate = self.model_binding_gate.write().await;
        self.ensure_running()?;
        let provider = self.provider(&selection.provider_instance_id)?;
        self.store
            .reset_model_fixed_config(selection)
            .await
            .map_err(|e| RuntimeError::from_store("reset fixed model", e))?;
        self.publish_provider_configuration(provider)
    }
    fn publish_provider_configuration(&self, provider: Arc<StoredProvider>) -> RuntimeResult<()> {
        // Arc 身份也代表该实例的固定设置接纳点。无目录／固定记录缓存，无额外 revision 字段；
        // 任一固定记录变更都会使该实例正在准备的请求重试，覆盖“原先无记录期间新增”。
        self.managed_models_mut()?.providers.insert(
            provider.provider_instance_id.clone(),
            Arc::new(provider.as_ref().clone()),
        );
        self.publish(RuntimeEvent::ConfigChanged);
        Ok(())
    }
    pub(super) fn provider(&self, id: &ProviderInstanceId) -> RuntimeResult<Arc<StoredProvider>> {
        self.managed_models()?
            .providers
            .get(id)
            .cloned()
            .ok_or_else(|| invalid("服务商不存在或已删除，请重新选择。"))
    }
    pub(super) fn ensure_provider_current(
        &self,
        captured: &Arc<StoredProvider>,
    ) -> RuntimeResult<()> {
        if self
            .managed_models()?
            .providers
            .get(&captured.provider_instance_id)
            .is_some_and(|current| Arc::ptr_eq(current, captured))
        {
            Ok(())
        } else {
            Err(RuntimeError::ConfigurationConflict)
        }
    }
    pub(super) async fn discover_provider(
        &self,
        provider: &StoredProvider,
    ) -> RuntimeResult<Vec<DiscoveredModel>> {
        let snapshot = self.config_registry.snapshot()?;
        let active = snapshot
            .active()
            .ok_or(RuntimeError::ConfigurationUnavailable)?;
        self.model_factory
            .discover_models(ModelDiscoveryRequest {
                format: provider.connection.discovery_format,
                models_path: &provider.connection.models_path,
                endpoint: &provider.connection.endpoint,
                api_key: provider.api_key.expose(),
                connect_timeout: active.transport().connect_timeout(),
                request_timeout: active.transport().request_timeout(),
            })
            .await
            .map_err(|_| invalid("获取服务商在线模型列表失败，请检查连接后重试。"))
    }
    fn managed_models(&self) -> RuntimeResult<RwLockReadGuard<'_, ManagedModels>> {
        self.config_registry.managed_models()
    }
    fn managed_models_mut(&self) -> RuntimeResult<RwLockWriteGuard<'_, ManagedModels>> {
        self.config_registry.managed_models_mut()
    }
}
fn invalid(reason: &'static str) -> RuntimeError {
    RuntimeError::InvalidRequest { reason }
}
fn summary(provider: &StoredProvider) -> ProviderSummary {
    ProviderSummary {
        provider_instance_id: provider.provider_instance_id.clone(),
        connection: provider.connection.clone(),
        has_api_key: !provider.api_key.expose().is_empty(),
    }
}
pub(super) fn validate_selection(selection: &ModelSelection) -> RuntimeResult<()> {
    if selection.model_id.trim().is_empty()
        || selection.model_id.len() > 1024
        || selection.model_id.chars().any(char::is_control)
    {
        return Err(invalid("模型 ID 为空、过长或含控制字符。"));
    }
    Ok(())
}
fn credential(
    previous: SecretValue,
    change: ProviderCredentialChange,
) -> RuntimeResult<SecretValue> {
    let value = match change {
        ProviderCredentialChange::Unchanged => previous,
        ProviderCredentialChange::Replace(value) => value,
        ProviderCredentialChange::Clear => SecretValue::new(String::new()),
    };
    if value.expose().len() > 16384 || value.expose().chars().any(char::is_control) {
        return Err(invalid("API Key 过长或含控制字符。"));
    }
    Ok(value)
}
fn validate_connection(connection: &ProviderConnection) -> RuntimeResult<()> {
    if connection.display_name.trim().is_empty()
        || connection.display_name.len() > 256
        || connection.display_name.chars().any(char::is_control)
    {
        return Err(invalid("请填写有效的服务商名称。"));
    }
    if connection.endpoint.len() > 8192 {
        return Err(invalid("服务地址过长。"));
    }
    let url = url::Url::parse(&connection.endpoint)
        .map_err(|_| invalid("服务地址必须是有效的 HTTP 或 HTTPS 地址。"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid("服务地址不允许 URL 凭据、查询参数或片段。"));
    }
    let path = &connection.models_path;
    if !path.is_empty()
        && (!path.starts_with('/')
            || path.starts_with("//")
            || path.contains(['?', '#', '%', '\\'])
            || path.split('/').any(|part| matches!(part, "." | ".."))
            || path.chars().any(char::is_control)
            || path.len() > 2048)
    {
        return Err(invalid(
            "模型列表路径必须是同源绝对路径，不含转义或查询参数。",
        ));
    }
    if connection.discovery_format != connection.provider_type.discovery_format() {
        return Err(invalid(
            "模型列表格式与服务商类型不匹配，请重新选择服务商类型。",
        ));
    }
    crate::config::resolve_provider_protocol(connection)?;
    Ok(())
}
fn validate_provider_parameters(
    connection: &ProviderConnection,
    parameters: &ModelParameters,
) -> RuntimeResult<()> {
    validate_connection(connection)?;
    use assistant_protocol::ModelToolImageProjection::*;
    if parameters.tool_image_projection == NativeToolResult
        && crate::config::resolve_provider_protocol(connection)?
            == crate::ModelProtocol::OpenAiChatCompletions
    {
        return Err(invalid(
            "Chat Completions 不支持工具结果原生图片，请调整固定配置。",
        ));
    }
    Ok(())
}

// 套餐凭据不能发到百炼原生列表端点；错误只说明接入类型，不回显凭据。
fn validate_provider_credential(
    connection: &ProviderConnection,
    key: &SecretValue,
) -> RuntimeResult<()> {
    if connection.provider_type == assistant_protocol::ProviderType::DashscopeApi
        && key.expose().starts_with("sk-sp-")
    {
        return Err(invalid(
            "套餐密钥应选择百炼套餐类型，不能用于百炼 API 原生接口。",
        ));
    }
    Ok(())
}
