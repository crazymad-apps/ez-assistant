use super::*;

impl AssistantRuntime {
    pub async fn get_model_configuration(
        &self,
        request: GetModelConfigurationRequest,
    ) -> RuntimeResult<ModelConfigurationDetail> {
        let selection = request.selection;
        validate_selection(&selection)?;
        self.ensure_running()?;
        let provider = self.provider(&selection.provider_instance_id)?;
        let fixed = self
            .config_registry
            .fixed_model(selection.clone(), self.store.as_ref())
            .await?;
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
        let model = catalog_model(&provider, &selection.model_id)?;
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
            .config_registry
            .fixed_models(
                request.provider_instance_id,
                request.offset,
                request.limit.clamp(1, 200),
                self.store.as_ref(),
            )
            .await?;
        self.ensure_provider_current(&provider)?;
        Ok(records)
    }

    pub async fn save_model_fixed_config(
        &self,
        request: SaveModelFixedConfigRequest,
    ) -> RuntimeResult<ModelFixedConfig> {
        self.ensure_model_editable()?;
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
        if request.origin == ModelConfigOrigin::Online && existing.is_none() {
            catalog_model(&provider, &request.selection.model_id)?;
        }
        let _operation = self.operation_gate.read().await;
        let _gate = self.model_binding_gate.write().await;
        self.ensure_running()?;
        self.ensure_provider_current(&provider)?;
        let fixed = ModelFixedConfig {
            origin: request.origin,
            selection: request.selection,
            parameters: request.parameters,
            updated_at_ms: super::super::now_ms()?,
        };
        self.store
            .put_model_fixed_config(fixed.clone())
            .await
            .map_err(|e| RuntimeError::from_store("save fixed model", e))?;
        self.publish_provider_configuration(provider)?;
        Ok(fixed)
    }

    pub async fn reset_model_fixed_config(&self, selection: ModelSelection) -> RuntimeResult<()> {
        self.ensure_model_editable()?;
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
}
