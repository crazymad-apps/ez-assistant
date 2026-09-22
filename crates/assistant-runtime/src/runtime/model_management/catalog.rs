use super::*;

const MODEL_CATALOG_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MODEL_CATALOG_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_MODEL_CATALOG_BYTES: usize = 8 * 1024 * 1024;

impl AssistantRuntime {
    pub async fn list_provider_models(
        &self,
        id: ProviderInstanceId,
    ) -> RuntimeResult<ProviderModelCatalogSnapshot> {
        self.ensure_running()?;
        let provider = self.provider(&id)?;
        Ok(catalog_snapshot(&provider))
    }

    /// 唯一目录联网入口。联网期间不持配置写门；可靠落库后才替换 Provider Arc 并发布失效事件。
    pub async fn refresh_provider_models(
        &self,
        id: ProviderInstanceId,
    ) -> RuntimeResult<ProviderModelCatalogSnapshot> {
        self.ensure_model_editable()?;
        self.ensure_running()?;
        let provider = self.provider(&id)?;
        let mut models = self
            .model_factory
            .discover_models(ModelDiscoveryRequest {
                format: provider.connection.discovery_format,
                models_path: &provider.connection.models_path,
                endpoint: &provider.connection.endpoint,
                api_key: provider.api_key.expose(),
                connect_timeout: MODEL_CATALOG_CONNECT_TIMEOUT,
                request_timeout: MODEL_CATALOG_REQUEST_TIMEOUT,
            })
            .await
            .map_err(|source| RuntimeError::ModelDiscoveryFailed { source })?;
        normalize_model_catalog(&mut models)?;
        let catalog = StoredModelCatalog {
            models,
            refreshed_at_ms: super::super::now_ms()?,
            connection_changed: false,
        };
        let _operation = self.operation_gate.read().await;
        let _gate = self.model_binding_gate.write().await;
        self.ensure_running()?;
        self.ensure_provider_current(&provider)?;
        self.store
            .replace_provider_model_catalog(ProviderModelCatalogReplacement {
                provider_instance_id: id.clone(),
                expected_connection: provider.connection.clone(),
                expected_api_key: provider.api_key.clone(),
                catalog: catalog.clone(),
            })
            .await
            .map_err(|error| {
                if error.kind() == StoreErrorKind::Conflict {
                    RuntimeError::ConfigurationConflict
                } else {
                    RuntimeError::from_store("replace provider model catalog", error)
                }
            })?;
        let refreshed = Arc::new(StoredProvider {
            model_catalog: Some(catalog),
            model_catalog_diagnostic: None,
            ..provider.as_ref().clone()
        });
        self.managed_models_mut()?
            .providers
            .insert(id, refreshed.clone());
        self.publish(RuntimeEvent::ConfigChanged);
        Ok(catalog_snapshot(&refreshed))
    }
}

fn catalog_snapshot(provider: &StoredProvider) -> ProviderModelCatalogSnapshot {
    let (mut models, refreshed_at_ms, connection_changed) =
        provider
            .model_catalog
            .as_ref()
            .map_or((Vec::new(), None, false), |catalog| {
                (
                    catalog.models.clone(),
                    Some(catalog.refreshed_at_ms),
                    catalog.connection_changed,
                )
            });
    for model in &mut models {
        let detail = crate::config::model_templates::configuration_detail(
            &provider.connection,
            ModelSelection {
                provider_instance_id: provider.provider_instance_id.clone(),
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
    ProviderModelCatalogSnapshot {
        provider_instance_id: provider.provider_instance_id.clone(),
        models,
        refreshed_at_ms,
        connection_changed,
        diagnostic: provider.model_catalog_diagnostic,
    }
}

pub(super) fn catalog_model(
    provider: &StoredProvider,
    model_id: &str,
) -> RuntimeResult<DiscoveredModel> {
    provider
        .model_catalog
        .as_ref()
        .and_then(|catalog| {
            catalog
                .models
                .iter()
                .find(|model| model.model_id == model_id)
        })
        .cloned()
        .ok_or_else(|| invalid("本地模型目录中没有该模型，请显式刷新目录或保存固定配置。"))
}

fn normalize_model_catalog(models: &mut [DiscoveredModel]) -> RuntimeResult<()> {
    for model in models.iter_mut() {
        // configuration 是读取时由当前模板派生的展示事实，不能持久化旧计算结果。
        model.configuration = None;
    }
    if !stored_model_catalog_is_valid(models) {
        return Err(invalid("模型目录内容无效或超过条目上限。"));
    }
    let encoded = serde_json::to_vec(models).map_err(|_| invalid("模型目录无法编码。"))?;
    if encoded.len() > MAX_MODEL_CATALOG_BYTES {
        return Err(invalid("模型目录编码后超过上限。"));
    }
    Ok(())
}
