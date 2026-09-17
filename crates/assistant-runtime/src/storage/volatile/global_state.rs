use super::*;

impl VolatileRuntimeStore {
    pub(super) fn search_conversation_titles(
        &self,
        request: ConversationSearchRequest,
    ) -> StoreFuture<'_, Vec<assistant_protocol::ConversationHistoryHit>> {
        Box::pin(async move {
            let state = self.lock()?;
            let query = request.query.to_lowercase();
            let mut hits = Vec::new();
            for session in state.sessions.values() {
                let included = match &request.scope {
                    ConversationSearchScope::Global => true,
                    ConversationSearchScope::Session { session_id } => {
                        &session.session_id == session_id
                    }
                    ConversationSearchScope::Workspace { workspace_id } => {
                        session.environment.workspace_id.as_ref() == Some(workspace_id)
                    }
                };
                if !included {
                    continue;
                }
                let lifecycle = match session.lifecycle {
                    StoredSessionLifecycle::Active => assistant_protocol::SessionLifecycle::Active,
                    StoredSessionLifecycle::Archived => {
                        assistant_protocol::SessionLifecycle::Archived
                    }
                };
                if session.title.to_lowercase().contains(&query) {
                    hits.push(assistant_protocol::ConversationHistoryHit {
                        owner: ConversationOwner::MainSession {
                            session_id: session.session_id.clone(),
                        },
                        session_title: session.title.clone(),
                        child_task_title: None,
                        message_id: None,
                        created_at_ms: Some(session.updated_at_ms),
                        snippet: session.title.clone(),
                        match_kind: assistant_protocol::ConversationHistoryMatchKind::Title,
                        lifecycle,
                    });
                }
                for child in state.child_tasks.values().filter(|c| {
                    c.session_id == session.session_id && c.title.to_lowercase().contains(&query)
                }) {
                    hits.push(assistant_protocol::ConversationHistoryHit {
                        owner: ConversationOwner::ChildTask {
                            session_id: session.session_id.clone(),
                            child_task_id: child.child_task_id.clone(),
                        },
                        session_title: session.title.clone(),
                        child_task_title: Some(child.title.clone()),
                        message_id: None,
                        created_at_ms: Some(child.finished_at_ms.unwrap_or(child.created_at_ms)),
                        snippet: child.title.clone(),
                        match_kind: assistant_protocol::ConversationHistoryMatchKind::Title,
                        lifecycle,
                    });
                }
            }
            hits.sort_by_key(|hit| std::cmp::Reverse(hit.created_at_ms));
            hits.truncate(request.limit.min(200));
            Ok(hits)
        })
    }

    pub(super) fn load_runtime_globals(&self) -> StoreFuture<'_, RecoveredRuntime> {
        Box::pin(async move {
            let state = self.lock()?;
            Ok(RecoveredRuntime {
                devices: state.devices.values().cloned().collect(),
                workspaces: state.workspaces.values().cloned().collect(),
                ..Default::default()
            })
        })
    }

    pub(super) fn prepare_session_execution(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, crate::LoadedSession> {
        let session_id = session_id.clone();
        Box::pin(async move {
            {
                let mut state = self.lock()?;
                let mut goals = BTreeMap::new();
                if let Some(goal) = state.goals.get(&session_id) {
                    goals.insert(session_id.clone(), goal.clone());
                }
                pause_running_goals_for_recovery(&mut goals)?;
                state.goals.extend(goals);
                let generation = state.goals.get(&session_id).map(|g| g.generation);
                let stale = state
                    .inputs
                    .values()
                    .filter(|i| {
                        i.session_id == session_id
                            && i.state == StoredInputState::Queued
                            && i.origin == InputOrigin::Runtime
                            && i.goal_binding
                                .as_ref()
                                .is_some_and(|b| generation.is_some_and(|g| b.generation < g))
                    })
                    .map(|i| i.input_id.clone())
                    .collect::<BTreeSet<_>>();
                state.inputs.retain(|id, _| !stale.contains(id));
                state.runs.retain(|_, r| !stale.contains(&r.input_id));
                state
                    .skill_activations
                    .retain(|_, a| a.input_id.as_ref().is_none_or(|id| !stale.contains(id)));
                state
                    .mcp_input_selections
                    .retain(|_, a| a.input_id.as_ref().is_none_or(|id| !stale.contains(id)));
            }
            self.load_session_state(&session_id).await
        })
    }

    pub(super) fn load_session_environment(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, crate::SessionExecutionEnvironment> {
        let session_id = session_id.clone();
        Box::pin(async move {
            self.lock()?
                .sessions
                .get(&session_id)
                .map(|s| s.environment.clone())
                .ok_or_else(|| conflict("session does not exist"))
        })
    }

    pub(super) fn load_session_state(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, crate::LoadedSession> {
        let session_id = session_id.clone();
        Box::pin(async move {
            let state = self.lock()?;
            Ok(crate::LoadedSession {
                identities: state
                    .sessions
                    .values()
                    .map(|s| (s.session_id.clone(), s.role, s.lifecycle))
                    .collect(),
                state: RecoveredRuntime {
                    devices: state.devices.values().cloned().collect(),
                    workspaces: state.workspaces.values().cloned().collect(),
                    attachments: state
                        .attachments
                        .values()
                        .filter(|row| row.session_id == session_id)
                        .cloned()
                        .collect(),
                    sessions: state
                        .sessions
                        .values()
                        .filter(|row| row.session_id == session_id)
                        .cloned()
                        .collect(),
                    inputs: state
                        .inputs
                        .values()
                        .filter(|row| row.session_id == session_id)
                        .cloned()
                        .collect(),
                    session_commands: state
                        .session_commands
                        .values()
                        .filter(|row| row.session_id == session_id)
                        .cloned()
                        .collect(),
                    mcp_input_selections: state
                        .mcp_input_selections
                        .values()
                        .filter(|row| row.session_id == session_id)
                        .cloned()
                        .collect(),
                    runs: state
                        .runs
                        .values()
                        .filter(|row| row.session_id == session_id)
                        .cloned()
                        .collect(),
                    child_tasks: state
                        .child_tasks
                        .values()
                        .filter(|row| row.session_id == session_id)
                        .cloned()
                        .collect(),
                    work_plans: state
                        .work_plans
                        .values()
                        .filter(|row| row.session_id == session_id)
                        .cloned()
                        .collect(),
                    goals: state
                        .goals
                        .values()
                        .filter(|row| row.session_id == session_id)
                        .cloned()
                        .collect(),
                    skill_activations: state
                        .skill_activations
                        .values()
                        .filter(|row| row.session_id == session_id)
                        .cloned()
                        .collect(),
                },
            })
        })
    }

    pub(super) fn query_session_summaries(
        &self,
        query: crate::SessionSummaryQuery,
    ) -> StoreFuture<'_, Vec<assistant_protocol::SessionSummary>> {
        Box::pin(async move {
            let state = self.lock()?;
            let mut rows = Vec::new();
            for stored in state.sessions.values() {
                if query
                    .session_id
                    .as_ref()
                    .is_some_and(|id| id != &stored.session_id)
                {
                    continue;
                }
                let mut row = crate::session::SessionController::new(stored.clone())
                    .summary()
                    .map_err(|_| conflict("session summary unavailable"))?;
                if query
                    .query
                    .as_ref()
                    .is_some_and(|q| !row.title.to_lowercase().contains(&q.to_lowercase()))
                {
                    continue;
                }
                if query.role.is_some_and(|role| role != row.role) {
                    continue;
                }
                if !match query.filter {
                    assistant_protocol::SessionListFilter::All => true,
                    assistant_protocol::SessionListFilter::Active => {
                        row.lifecycle == assistant_protocol::SessionLifecycle::Active
                    }
                    assistant_protocol::SessionListFilter::Archived => {
                        row.lifecycle == assistant_protocol::SessionLifecycle::Archived
                    }
                } {
                    continue;
                }
                row.queued_input_count = state
                    .inputs
                    .values()
                    .filter(|i| {
                        i.session_id == row.session_id
                            && i.state == StoredInputState::Queued
                            && i.origin == InputOrigin::User
                            && i.goal_binding.is_none()
                    })
                    .count() as u64;
                row.resume_required = row.queued_input_count > 0
                    || state.session_commands.values().any(|c| {
                        c.session_id == row.session_id
                            && c.state == super::StoredSessionCommandState::Queued
                    });
                rows.push(row);
            }
            rows.sort_by(|a, b| {
                b.is_pinned
                    .cmp(&a.is_pinned)
                    .then_with(|| b.updated_at_ms.cmp(&a.updated_at_ms))
                    .then_with(|| a.session_id.cmp(&b.session_id))
            });
            Ok(rows
                .into_iter()
                .skip(query.offset as usize)
                .take(query.limit.clamp(1, 201) as usize)
                .collect())
        })
    }

    pub(super) fn load_runtime(&self) -> StoreFuture<'_, RecoveredRuntime> {
        Box::pin(async move {
            let mut state = self.lock()?;
            state.work_plans.retain(|_, plan| {
                plan.items.is_empty()
                    || plan
                        .items
                        .iter()
                        .any(|item| item.status != StoredTodoItemStatus::Completed)
            });
            state
                .goals
                .retain(|_, goal| goal.state != StoredGoalState::Completed);
            pause_running_goals_for_recovery(&mut state.goals)?;
            let stale_goal_inputs = state
                .inputs
                .values()
                .filter(|input| {
                    input.state == StoredInputState::Queued
                        && input.origin == InputOrigin::Runtime
                        && input.goal_binding.as_ref().is_some_and(|binding| {
                            state.goals.get(&input.session_id).is_some_and(|goal| {
                                goal.goal_id == binding.goal_id
                                    && binding.generation < goal.generation
                            })
                        })
                })
                .map(|input| input.input_id.clone())
                .collect::<BTreeSet<_>>();
            state
                .inputs
                .retain(|input_id, _| !stale_goal_inputs.contains(input_id));
            state
                .runs
                .retain(|_, run| !stale_goal_inputs.contains(&run.input_id));
            state.skill_activations.retain(|_, activation| {
                activation
                    .input_id
                    .as_ref()
                    .is_none_or(|input_id| !stale_goal_inputs.contains(input_id))
            });
            state.mcp_input_selections.retain(|_, selection| {
                selection
                    .input_id
                    .as_ref()
                    .is_none_or(|input_id| !stale_goal_inputs.contains(input_id))
            });
            Ok(RecoveredRuntime {
                devices: state.devices.values().cloned().collect(),
                workspaces: state.workspaces.values().cloned().collect(),
                attachments: state.attachments.values().cloned().collect(),
                sessions: state.sessions.values().cloned().collect(),
                inputs: state.inputs.values().cloned().collect(),
                session_commands: state.session_commands.values().cloned().collect(),
                mcp_input_selections: state.mcp_input_selections.values().cloned().collect(),
                runs: state.runs.values().cloned().collect(),
                child_tasks: state.child_tasks.values().cloned().collect(),
                work_plans: state.work_plans.values().cloned().collect(),
                goals: state.goals.values().cloned().collect(),
                skill_activations: state.skill_activations.values().cloned().collect(),
            })
        })
    }

    pub(super) fn register_paired_device(
        &self,
        device: NewPairedDevice,
    ) -> StoreFuture<'_, PairedDevice> {
        Box::pin(async move {
            if device.display_name.trim().is_empty() {
                return Err(conflict("device display name is empty"));
            }
            let mut state = self.lock()?;
            if let Some(existing) = state.devices.get(&device.device_id) {
                if existing.lifecycle == DeviceLifecycle::Paired
                    && existing.public_key == device.public_key
                {
                    return Ok(existing.clone());
                }
                return Err(conflict("device identity already exists"));
            }
            if state
                .devices
                .values()
                .any(|existing| existing.public_key == device.public_key)
            {
                return Err(conflict("device public key already exists"));
            }
            let stored = PairedDevice {
                device_id: device.device_id.clone(),
                display_name: device.display_name,
                public_key: device.public_key,
                lifecycle: DeviceLifecycle::Paired,
                paired_at_ms: device.paired_at_ms,
                updated_at_ms: device.paired_at_ms,
                revoked_at_ms: None,
            };
            state.devices.insert(device.device_id, stored.clone());
            Ok(stored)
        })
    }

    pub(super) fn rename_device(&self, change: DeviceNameChange) -> StoreFuture<'_, PairedDevice> {
        Box::pin(async move {
            if change.display_name.trim().is_empty() {
                return Err(conflict("device display name is empty"));
            }
            let mut state = self.lock()?;
            let device = state
                .devices
                .get_mut(&change.device_id)
                .ok_or_else(|| conflict("device does not exist"))?;
            if device.lifecycle != DeviceLifecycle::Paired {
                return Err(conflict("device is revoked"));
            }
            device.display_name = change.display_name.clone();
            device.updated_at_ms = change.changed_at_ms;
            let device = device.clone();
            for session in state.sessions.values_mut() {
                if let Some(hosting) = session.pc_output_hosting.as_mut()
                    && hosting.device_id == change.device_id
                {
                    hosting.device_name = change.display_name.clone();
                }
            }
            Ok(device)
        })
    }

    pub(super) fn revoke_device(
        &self,
        change: DeviceRevocation,
    ) -> StoreFuture<'_, DeviceRevocationResult> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let device = state
                .devices
                .get_mut(&change.device_id)
                .ok_or_else(|| conflict("device does not exist"))?;
            let changed = device.lifecycle == DeviceLifecycle::Paired;
            if changed {
                device.lifecycle = DeviceLifecycle::Revoked;
                device.updated_at_ms = change.revoked_at_ms;
                device.revoked_at_ms = Some(change.revoked_at_ms);
            }
            let device = device.clone();
            let mut cleared_session_ids = Vec::new();
            for session in state.sessions.values_mut() {
                if session
                    .pc_output_hosting
                    .as_ref()
                    .is_some_and(|hosting| hosting.device_id == change.device_id)
                {
                    session.pc_output_hosting = None;
                    cleared_session_ids.push(session.session_id.clone());
                }
            }
            Ok(DeviceRevocationResult {
                device,
                cleared_session_ids,
                changed,
            })
        })
    }

    pub(super) fn set_pc_output_hosting(
        &self,
        change: PcOutputHostingChange,
    ) -> StoreFuture<'_, bool> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let target = change
                .device_id
                .as_ref()
                .map(|device_id| {
                    state
                        .devices
                        .get(device_id)
                        .filter(|device| device.lifecycle == DeviceLifecycle::Paired)
                        .cloned()
                        .ok_or_else(|| conflict("hosting device is not paired"))
                })
                .transpose()?;
            let session = state
                .sessions
                .get_mut(&change.controller_session_id)
                .ok_or_else(|| conflict("controller session does not exist"))?;
            if session.role != SessionRole::Controller {
                return Err(conflict("hosting target session is not controller"));
            }
            let next = target.map(|device| PcOutputHosting {
                device_id: device.device_id,
                device_name: device.display_name,
            });
            if session.pc_output_hosting == next {
                return Ok(false);
            }
            session.pc_output_hosting = next;
            Ok(true)
        })
    }

    pub(super) fn list_skill_name_states(&self) -> StoreFuture<'_, Vec<SkillNameState>> {
        Box::pin(async move { Ok(self.lock()?.skill_name_states.values().cloned().collect()) })
    }

    pub(super) fn set_skill_enabled(
        &self,
        change: SkillNameStateChange,
    ) -> StoreFuture<'_, SkillNameState> {
        Box::pin(async move {
            let state = SkillNameState {
                name: change.name,
                enabled: change.enabled,
                updated_at_ms: change.updated_at_ms,
            };
            self.lock()?
                .skill_name_states
                .insert(state.name.clone(), state.clone());
            Ok(state)
        })
    }

    pub(super) fn load_memory_context(&self) -> StoreFuture<'_, MemoryContextSnapshot> {
        Box::pin(async move {
            let state = self.lock()?;
            Ok(MemoryContextSnapshot {
                persona: state.persona.clone(),
                pinned_collection_revision: state.pinned_collection_revision,
                pinned_memories: state.pinned_memories.values().cloned().collect(),
            })
        })
    }

    pub(super) fn get_persona(&self) -> StoreFuture<'_, PersonaSnapshot> {
        Box::pin(async move { Ok(self.lock()?.persona.clone()) })
    }

    pub(super) fn set_persona(
        &self,
        mutation: PersonaMutation,
    ) -> StoreFuture<'_, PersonaSnapshot> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if state.persona.revision != mutation.expected_revision {
                return Err(conflict("persona revision changed"));
            }
            let next_revision = mutation
                .expected_revision
                .checked_add(1)
                .ok_or_else(|| conflict("persona revision exhausted"))?;
            state.persona = PersonaSnapshot {
                enabled: mutation.enabled,
                content: mutation.content,
                revision: next_revision,
                updated_at_ms: mutation.updated_at_ms,
            };
            Ok(state.persona.clone())
        })
    }

    pub(super) fn list_pinned_memories(&self) -> StoreFuture<'_, Vec<StoredPinnedMemory>> {
        Box::pin(async move { Ok(self.lock()?.pinned_memories.values().cloned().collect()) })
    }

    pub(super) fn mutate_pinned_memory(
        &self,
        mutation: PinnedMemoryMutation,
    ) -> StoreFuture<'_, PinnedMemoryMutationResult> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let memory = match mutation {
                PinnedMemoryMutation::Create {
                    entry,
                    created_by,
                    expected_collection_revision,
                    changed_at_ms,
                } => {
                    if state.pinned_collection_revision != expected_collection_revision {
                        return Err(conflict("pinned memory collection revision changed"));
                    }
                    if state.pinned_memories.contains_key(entry.id.as_str()) {
                        return Err(conflict("pinned memory already exists"));
                    }
                    let stored = StoredPinnedMemory {
                        entry,
                        created_by,
                        created_at_ms: changed_at_ms,
                        updated_at_ms: changed_at_ms,
                        revision: 1,
                    };
                    state
                        .pinned_memories
                        .insert(stored.entry.id.as_str().to_owned(), stored.clone());
                    Some(stored)
                }
                PinnedMemoryMutation::Replace {
                    entry,
                    expected_revision,
                    changed_at_ms,
                } => {
                    let existing = state
                        .pinned_memories
                        .get_mut(entry.id.as_str())
                        .ok_or_else(|| conflict("pinned memory does not exist"))?;
                    if existing.revision != expected_revision {
                        return Err(conflict("pinned memory revision changed"));
                    }
                    let next_revision = existing
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| conflict("pinned memory revision exhausted"))?;
                    existing.entry = entry;
                    existing.updated_at_ms = changed_at_ms;
                    existing.revision = next_revision;
                    Some(existing.clone())
                }
                PinnedMemoryMutation::Delete {
                    id,
                    expected_revision,
                    changed_at_ms: _,
                } => {
                    let existing = state
                        .pinned_memories
                        .get(id.as_str())
                        .ok_or_else(|| conflict("pinned memory does not exist"))?;
                    if existing.revision != expected_revision {
                        return Err(conflict("pinned memory revision changed"));
                    }
                    state.pinned_memories.remove(id.as_str());
                    None
                }
            };
            state.pinned_collection_revision = state
                .pinned_collection_revision
                .checked_add(1)
                .ok_or_else(|| conflict("pinned memory collection revision exhausted"))?;
            Ok(PinnedMemoryMutationResult {
                memory,
                collection_revision: state.pinned_collection_revision,
            })
        })
    }
}
