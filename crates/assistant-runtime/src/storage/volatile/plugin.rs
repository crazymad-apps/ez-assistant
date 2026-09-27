use super::*;

impl VolatileRuntimeStore {
    pub(super) fn list_plugins(&self) -> StoreFuture<'_, Vec<StoredPlugin>> {
        Box::pin(async move { Ok(self.lock()?.plugins.values().cloned().collect()) })
    }

    pub(super) fn change_plugin_settings(
        &self,
        change: PluginSettingsChange,
    ) -> StoreFuture<'_, StoredPlugin> {
        Box::pin(async move {
            change.validate()?;
            let mut state = self.lock()?;
            let current = state.plugins.get(&change.expected.name).ok_or_else(|| {
                StoreError::new(StoreErrorKind::Conflict, "plugin is unavailable")
            })?;
            if current != &change.expected {
                return Err(StoreError::new(
                    StoreErrorKind::Conflict,
                    "plugin settings changed",
                ));
            }
            state
                .plugins
                .insert(change.next.name.clone(), change.next.clone());
            Ok(change.next)
        })
    }

    pub(super) fn list_plugin_operations(&self) -> StoreFuture<'_, Vec<StoredPluginOperation>> {
        Box::pin(async move { Ok(self.lock()?.plugin_operations.values().cloned().collect()) })
    }

    pub(super) fn accept_plugin_operation(
        &self,
        operation: StoredPluginOperation,
    ) -> StoreFuture<'_, StoredPluginOperation> {
        Box::pin(async move {
            operation.validate()?;
            if operation.phase != PluginOperationPhase::Accepted
                || operation.committed_json != "[]"
                || operation.cancel_requested
                || operation.error_code.is_some()
            {
                return Err(StoreError::new(
                    StoreErrorKind::InvalidInput,
                    "plugin operation initial state is invalid",
                ));
            }
            let mut state = self.lock()?;
            if let Some(existing) = state.plugin_operations.get(&operation.key) {
                if existing.kind == operation.kind && existing.intent_json == operation.intent_json
                {
                    return Ok(existing.clone());
                }
                return Err(StoreError::new(
                    StoreErrorKind::Conflict,
                    "plugin operation key conflicts with another intent",
                ));
            }
            state
                .plugin_operations
                .insert(operation.key.clone(), operation.clone());
            Ok(operation)
        })
    }

    pub(super) fn advance_plugin_operation(
        &self,
        update: PluginOperationUpdate,
    ) -> StoreFuture<'_, StoredPluginOperation> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let current = state
                .plugin_operations
                .get(&update.next.key)
                .ok_or_else(|| {
                    StoreError::new(StoreErrorKind::Conflict, "plugin operation does not exist")
                })?;
            if current == &update.next {
                return Ok(current.clone());
            }
            update.validate_against(current)?;
            if let PluginRecordChange::Upsert(plugin) = &update.plugin
                && let Some(existing) = state.plugins.get(&plugin.name)
                && (existing.source != plugin.source
                    || existing.created_at_ms != plugin.created_at_ms)
            {
                return Err(StoreError::new(
                    StoreErrorKind::Conflict,
                    "plugin source or creation time changed",
                ));
            }
            match update.plugin {
                PluginRecordChange::Keep => {}
                PluginRecordChange::Upsert(plugin) => {
                    state.plugins.insert(plugin.name.clone(), plugin);
                }
                PluginRecordChange::Remove(name) => {
                    state.plugins.remove(&name);
                }
            }
            state
                .plugin_operations
                .insert(update.next.key.clone(), update.next.clone());
            Ok(update.next)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn operation_replay_and_plugin_commit_match_persistent_contract() {
        let store = VolatileRuntimeStore::default();
        let operation = StoredPluginOperation {
            key: IdempotencyKey::new("example").unwrap(),
            kind: super::super::super::PluginOperationKind::Install,
            intent_json: r#"{"name":"example"}"#.into(),
            phase: PluginOperationPhase::Accepted,
            committed_json: "[]".into(),
            cancel_requested: false,
            error_code: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        store
            .accept_plugin_operation(operation.clone())
            .await
            .unwrap();
        assert_eq!(
            store
                .accept_plugin_operation(operation.clone())
                .await
                .unwrap(),
            operation
        );
        let mut next = operation.clone();
        next.phase = PluginOperationPhase::Preparing;
        next.updated_at_ms = 2;
        store
            .advance_plugin_operation(PluginOperationUpdate {
                expected_phase: PluginOperationPhase::Accepted,
                next: next.clone(),
                plugin: PluginRecordChange::Keep,
            })
            .await
            .unwrap();
        let mut premature = next.clone();
        premature.phase = PluginOperationPhase::Cleaning;
        premature.updated_at_ms = 3;
        let plugin = StoredPlugin {
            name: "example".into(),
            source: super::super::super::PluginSource::Upload,
            active_version: Some("1.0.0".into()),
            active_sha256: Some("a".repeat(64)),
            active_manifest_json: Some("{}".into()),
            enabled: true,
            parameters_json: "{}".into(),
            software_paths_json: "{}".into(),
            market_status: None,
            created_at_ms: 1,
            updated_at_ms: 3,
        };
        assert_eq!(
            store
                .advance_plugin_operation(PluginOperationUpdate {
                    expected_phase: PluginOperationPhase::Preparing,
                    next: premature,
                    plugin: PluginRecordChange::Upsert(plugin.clone()),
                })
                .await
                .unwrap_err()
                .kind(),
            StoreErrorKind::Conflict
        );
        assert!(store.list_plugins().await.unwrap().is_empty());
        let mut committing = next.clone();
        committing.phase = PluginOperationPhase::Committing;
        committing.updated_at_ms = 3;
        store
            .advance_plugin_operation(PluginOperationUpdate {
                expected_phase: PluginOperationPhase::Preparing,
                next: committing.clone(),
                plugin: PluginRecordChange::Keep,
            })
            .await
            .unwrap();
        let mut cleaning = committing.clone();
        cleaning.phase = PluginOperationPhase::Cleaning;
        cleaning.updated_at_ms = 4;
        store
            .advance_plugin_operation(PluginOperationUpdate {
                expected_phase: PluginOperationPhase::Committing,
                next: cleaning.clone(),
                plugin: PluginRecordChange::Upsert(plugin.clone()),
            })
            .await
            .unwrap();
        assert_eq!(store.list_plugins().await.unwrap(), vec![plugin]);
        assert_eq!(
            store.list_plugin_operations().await.unwrap(),
            vec![cleaning]
        );
    }
}
