use super::*;

impl VolatileRuntimeStore {
    pub(super) fn list_custom_agents(&self) -> StoreFuture<'_, Vec<StoredCustomAgent>> {
        Box::pin(async move { Ok(self.lock()?.custom_agents.values().cloned().collect()) })
    }

    pub(super) fn mutate_custom_agent(
        &self,
        mutation: CustomAgentMutation,
    ) -> StoreFuture<'_, Option<StoredCustomAgent>> {
        Box::pin(async move {
            let mut state = self.lock()?;
            match mutation {
                CustomAgentMutation::Save {
                    expected_revision,
                    next,
                } => {
                    if !crate::valid_custom_agent_config(&next.config)
                        || next.created_at_ms > next.updated_at_ms
                    {
                        return Err(StoreError::new(
                            StoreErrorKind::InvalidInput,
                            "custom agent config is invalid",
                        ));
                    }
                    let current = state.custom_agents.get(&next.config.name);
                    if current
                        .as_ref()
                        .map(|agent| crate::custom_agent_revision(&agent.config))
                        != expected_revision
                        || current.is_some_and(|agent| agent.created_at_ms != next.created_at_ms)
                    {
                        return Err(StoreError::new(
                            StoreErrorKind::Conflict,
                            "custom agent config changed",
                        ));
                    }
                    state
                        .custom_agents
                        .insert(next.config.name.clone(), next.clone());
                    Ok(Some(next))
                }
                CustomAgentMutation::Delete {
                    name,
                    expected_revision,
                } => {
                    let current = state.custom_agents.get(&name).ok_or_else(|| {
                        StoreError::new(StoreErrorKind::Conflict, "custom agent is unavailable")
                    })?;
                    if crate::custom_agent_revision(&current.config) != expected_revision {
                        return Err(StoreError::new(
                            StoreErrorKind::Conflict,
                            "custom agent config changed",
                        ));
                    }
                    state.custom_agents.remove(&name);
                    Ok(None)
                }
            }
        })
    }
}
