use super::*;

impl AssistantRuntime {
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
            model_catalog: None,
            model_catalog_diagnostic: None,
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
        let mut model_catalog = old.model_catalog.clone();
        if discovery_connection_changed(&old, &request.connection, &api_key)
            && let Some(catalog) = &mut model_catalog
        {
            catalog.connection_changed = true;
        }
        let provider = Arc::new(StoredProvider {
            provider_instance_id: request.provider_instance_id.clone(),
            connection: request.connection,
            api_key,
            model_catalog,
            model_catalog_diagnostic: old.model_catalog_diagnostic,
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
}

fn summary(provider: &StoredProvider) -> ProviderSummary {
    ProviderSummary {
        provider_instance_id: provider.provider_instance_id.clone(),
        connection: provider.connection.clone(),
        has_api_key: !provider.api_key.expose().is_empty(),
    }
}

fn discovery_connection_changed(
    previous: &StoredProvider,
    next: &ProviderConnection,
    next_api_key: &SecretValue,
) -> bool {
    previous.connection.provider_type != next.provider_type
        || previous.connection.endpoint != next.endpoint
        || previous.connection.models_path != next.models_path
        || previous.connection.discovery_format != next.discovery_format
        || previous.api_key != *next_api_key
}
