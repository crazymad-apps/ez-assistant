use super::*;

impl VolatileRuntimeStore {
    pub(super) fn load_default_agent_shell(
        &self,
    ) -> StoreFuture<'_, Option<assistant_protocol::ShellKind>> {
        Box::pin(async move { Ok(self.lock()?.default_agent_shell) })
    }

    pub(super) fn save_default_agent_shell(
        &self,
        kind: assistant_protocol::ShellKind,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.lock()?.default_agent_shell = Some(kind);
            Ok(())
        })
    }

    pub(super) fn load_providers(&self) -> StoreFuture<'_, Vec<crate::StoredProvider>> {
        Box::pin(async move { Ok(self.lock()?.providers.values().cloned().collect()) })
    }

    pub(super) fn put_provider(&self, provider: crate::StoredProvider) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.lock()?
                .providers
                .insert(provider.provider_instance_id.clone(), provider);
            Ok(())
        })
    }

    pub(super) fn replace_provider_model_catalog(
        &self,
        replacement: crate::ProviderModelCatalogReplacement,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let provider = state
                .providers
                .get_mut(&replacement.provider_instance_id)
                .ok_or_else(|| {
                    StoreError::new(StoreErrorKind::Conflict, "provider changed during refresh")
                })?;
            if provider.connection != replacement.expected_connection
                || provider.api_key != replacement.expected_api_key
            {
                return Err(StoreError::new(
                    StoreErrorKind::Conflict,
                    "provider changed during refresh",
                ));
            }
            provider.model_catalog = Some(replacement.catalog);
            provider.model_catalog_diagnostic = None;
            Ok(())
        })
    }

    pub(super) fn remove_provider(
        &self,
        id: assistant_protocol::ProviderInstanceId,
    ) -> StoreFuture<'_, assistant_protocol::ProviderUsage> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let usage = state.provider_usage(&id);
            state.providers.remove(&id);
            state
                .fixed_models
                .retain(|key, _| key.provider_instance_id != id);
            Ok(usage)
        })
    }

    pub(super) fn provider_usage(
        &self,
        id: assistant_protocol::ProviderInstanceId,
    ) -> StoreFuture<'_, assistant_protocol::ProviderUsage> {
        Box::pin(async move { Ok(self.lock()?.provider_usage(&id)) })
    }

    pub(super) fn load_model_settings(&self) -> StoreFuture<'_, assistant_protocol::ModelSettings> {
        Box::pin(async move { Ok(self.lock()?.model_settings.clone()) })
    }

    pub(super) fn save_model_settings(
        &self,
        settings: assistant_protocol::ModelSettings,
        server_history_key: Option<String>,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if let Some(key) = server_history_key {
                for session in state
                    .sessions
                    .values_mut()
                    .filter(|s| s.model_selection.is_none())
                {
                    session.server_history_key = Some(key.clone());
                }
            }
            state.model_settings = settings;
            Ok(())
        })
    }

    pub(super) fn invalidate_server_history_bindings(&self, key: String) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            if key.is_empty() {
                return Err(StoreError::new(
                    StoreErrorKind::InvalidData,
                    "history binding is empty",
                ));
            }
            for session in self.lock()?.sessions.values_mut() {
                session.server_history_key = Some(key.clone());
            }
            Ok(())
        })
    }

    pub(super) fn get_model_fixed_config(
        &self,
        selection: assistant_protocol::ModelSelection,
    ) -> StoreFuture<'_, Option<assistant_protocol::ModelFixedConfig>> {
        Box::pin(async move { Ok(self.lock()?.fixed_models.get(&selection).cloned()) })
    }

    pub(super) fn list_model_fixed_configs(
        &self,
        id: assistant_protocol::ProviderInstanceId,
        offset: u32,
        limit: u32,
    ) -> StoreFuture<'_, Vec<assistant_protocol::ModelFixedConfig>> {
        Box::pin(async move {
            Ok(self
                .lock()?
                .fixed_models
                .values()
                .filter(|config| config.selection.provider_instance_id == id)
                .skip(offset as usize)
                .take(limit.min(200) as usize)
                .cloned()
                .collect())
        })
    }

    pub(super) fn put_model_fixed_config(
        &self,
        config: assistant_protocol::ModelFixedConfig,
        server_history_key: Option<String>,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            crate::validate_fixed_model_parameters(&config.parameters).map_err(|_| {
                StoreError::new(
                    StoreErrorKind::InvalidData,
                    "fixed model parameters are inconsistent",
                )
            })?;
            let mut state = self.lock()?;
            if !state
                .providers
                .contains_key(&config.selection.provider_instance_id)
            {
                return Err(StoreError::new(
                    StoreErrorKind::Conflict,
                    "provider does not exist",
                ));
            }
            if state
                .fixed_models
                .get(&config.selection)
                .is_some_and(|existing| existing.origin != config.origin)
            {
                return Err(StoreError::new(
                    StoreErrorKind::Conflict,
                    "model origin cannot change",
                ));
            }
            invalidate_fixed_history(&mut state, &config.selection, server_history_key);
            state.fixed_models.insert(config.selection.clone(), config);
            Ok(())
        })
    }

    pub(super) fn reset_model_fixed_config(
        &self,
        selection: assistant_protocol::ModelSelection,
        server_history_key: Option<String>,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            invalidate_fixed_history(&mut state, &selection, server_history_key);
            state.fixed_models.remove(&selection);
            Ok(())
        })
    }
}

/// 与固定参数的写入共享同一锁；默认跟随者只受当前默认模型影响。
fn invalidate_fixed_history(
    state: &mut State,
    selection: &assistant_protocol::ModelSelection,
    key: Option<String>,
) {
    let Some(key) = key else {
        return;
    };
    for session in state.sessions.values_mut() {
        if session
            .model_selection
            .as_ref()
            .or(state.model_settings.default_model.as_ref())
            == Some(selection)
        {
            session.server_history_key = Some(key.clone());
        }
    }
}
