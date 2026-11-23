use super::*;

impl RuntimeStore for VolatileRuntimeStore {
    fn next_schedule_maintenance(
        &self,
        after: Option<String>,
    ) -> StoreFuture<'_, Option<(String, crate::ScheduleMaintenance)>> {
        Box::pin(async move { self.next_schedule_maintenance_inner(after) })
    }

    fn mark_schedule_deleting(&self, id: assistant_protocol::ScheduleId) -> StoreFuture<'_, bool> {
        Box::pin(async move { self.mark_schedule_deleting_inner(id) })
    }

    fn archive_schedule_result(
        &self,
        id: assistant_protocol::ScheduleExecutionId,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move { self.archive_schedule_result_inner(id) })
    }

    fn commit_schedule_report(
        &self,
        commit: crate::ScheduleReportCommit,
    ) -> StoreFuture<'_, Option<crate::AcceptedInput>> {
        Box::pin(self.commit_schedule_report_inner(commit))
    }

    fn finish_schedule_result(
        &self,
        id: assistant_protocol::ScheduleExecutionId,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move { self.finish_schedule_result_inner(id) })
    }

    fn resolve_schedule_file(
        &self,
        id: assistant_protocol::ScheduleExecutionId,
        file_id: String,
    ) -> StoreFuture<'_, Option<crate::ResolvedScheduleFile>> {
        Box::pin(async move { self.resolve_schedule_file_inner(id, file_id) })
    }

    fn next_schedule_due(&self) -> StoreFuture<'_, Option<crate::StoredSchedule>> {
        Box::pin(async move { self.next_schedule_due_inner() })
    }

    fn admit_schedule(
        &self,
        command: crate::ScheduleAdmission,
    ) -> StoreFuture<'_, crate::ScheduleAdmitted> {
        Box::pin(async move { self.admit_schedule_inner(command) })
    }

    fn settle_schedule_execution(
        &self,
        settlement: crate::ScheduleSettlement,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move { self.settle_schedule_execution_inner(settlement) })
    }

    fn recover_schedule_executions(
        &self,
        now: i64,
        _cleanup_verified: bool,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move { self.recover_schedule_executions_inner(now) })
    }

    fn get_schedule_execution(
        &self,
        id: assistant_protocol::ScheduleExecutionId,
    ) -> StoreFuture<'_, Option<assistant_protocol::ScheduleExecution>> {
        Box::pin(async move { self.get_schedule_execution_inner(id) })
    }

    fn list_schedule_executions(
        &self,
        id: assistant_protocol::ScheduleId,
        offset: u32,
        limit: u32,
    ) -> StoreFuture<'_, Option<assistant_protocol::ListScheduleExecutionsResult>> {
        Box::pin(async move { self.list_schedule_executions_inner(id, offset, limit) })
    }

    fn list_schedules(
        &self,
        offset: u32,
        limit: u32,
    ) -> StoreFuture<'_, crate::StoredSchedulePage> {
        Box::pin(async move {
            let state = self.lock()?;
            let schedules = state
                .schedules
                .values()
                .skip(offset as usize)
                .take(limit as usize)
                .cloned()
                .collect();
            Ok(crate::StoredSchedulePage {
                schedules,
                page: assistant_protocol::ContextPage {
                    offset,
                    limit,
                    total: u32::try_from(state.schedules.len()).unwrap_or(u32::MAX),
                },
            })
        })
    }

    fn get_schedule(
        &self,
        id: assistant_protocol::ScheduleId,
    ) -> StoreFuture<'_, Option<crate::StoredSchedule>> {
        Box::pin(async move { Ok(self.lock()?.schedules.get(&id).cloned()) })
    }

    fn insert_schedule(&self, schedule: crate::StoredSchedule) -> StoreFuture<'_, bool> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let std::collections::btree_map::Entry::Vacant(entry) =
                state.schedules.entry(schedule.schedule_id.clone())
            else {
                return Ok(false);
            };
            entry.insert(schedule);
            Ok(true)
        })
    }

    fn replace_schedule(&self, schedule: crate::StoredSchedule) -> StoreFuture<'_, bool> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let Some(previous) = state.schedules.get_mut(&schedule.schedule_id) else {
                return Ok(false);
            };
            if previous.deleting {
                return Ok(false);
            }
            *previous = schedule;
            Ok(true)
        })
    }

    fn delete_schedule(&self, id: assistant_protocol::ScheduleId) -> StoreFuture<'_, bool> {
        Box::pin(async move { self.remove_schedule(id) })
    }

    fn publish_message_card(
        &self,
        card: crate::NewMessageCard,
    ) -> StoreFuture<'_, crate::StoredMessageCard> {
        VolatileRuntimeStore::publish_message_card(self, card)
    }
    fn list_message_cards(
        &self,
        request: assistant_protocol::ListMessageCardsRequest,
    ) -> StoreFuture<'_, assistant_protocol::ListMessageCardsResult> {
        VolatileRuntimeStore::list_message_cards(self, request)
    }
    fn get_message_card(
        &self,
        owner: assistant_protocol::ConversationOwner,
        card_id: String,
    ) -> StoreFuture<'_, Option<crate::StoredMessageCard>> {
        VolatileRuntimeStore::get_message_card(self, owner, card_id)
    }
    fn save_message_card_snapshot(
        &self,
        write: crate::MessageCardSnapshotWrite,
    ) -> StoreFuture<'_, assistant_protocol::SaveMessageCardSnapshotResult> {
        VolatileRuntimeStore::save_message_card_snapshot(self, write)
    }
    fn queued_input_ids(&self) -> StoreFuture<'_, Vec<assistant_protocol::InputId>> {
        Box::pin(async move {
            let state = self.lock()?;
            Ok(state
                .inputs
                .values()
                .filter(|input| input.state == StoredInputState::Queued)
                .map(|input| input.input_id.clone())
                .chain(
                    state
                        .session_commands
                        .values()
                        .filter(|command| command.state == StoredSessionCommandState::Queued)
                        .map(|command| command.input_id.clone()),
                )
                .collect())
        })
    }

    fn load_conversation_history(
        &self,
        owner: ConversationOwner,
    ) -> StoreFuture<'_, super::super::StoredConversationHistory> {
        VolatileRuntimeStore::load_conversation_history(self, owner)
    }
    fn load_feedback(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, Vec<assistant_protocol::FeedbackRequestSnapshot>> {
        VolatileRuntimeStore::load_feedback(self, session_id)
    }
    fn mutate_feedback(&self, mutation: crate::FeedbackMutation) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::mutate_feedback(self, mutation)
    }

    fn load_default_agent_shell(&self) -> StoreFuture<'_, Option<assistant_protocol::ShellKind>> {
        VolatileRuntimeStore::load_default_agent_shell(self)
    }

    fn save_default_agent_shell(&self, kind: assistant_protocol::ShellKind) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::save_default_agent_shell(self, kind)
    }

    fn load_providers(&self) -> StoreFuture<'_, Vec<crate::StoredProvider>> {
        VolatileRuntimeStore::load_providers(self)
    }

    fn put_provider(&self, provider: crate::StoredProvider) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::put_provider(self, provider)
    }

    fn replace_provider_model_catalog(
        &self,
        replacement: crate::ProviderModelCatalogReplacement,
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::replace_provider_model_catalog(self, replacement)
    }

    fn remove_provider(
        &self,
        id: assistant_protocol::ProviderInstanceId,
    ) -> StoreFuture<'_, assistant_protocol::ProviderUsage> {
        VolatileRuntimeStore::remove_provider(self, id)
    }

    fn provider_usage(
        &self,
        id: assistant_protocol::ProviderInstanceId,
    ) -> StoreFuture<'_, assistant_protocol::ProviderUsage> {
        VolatileRuntimeStore::provider_usage(self, id)
    }

    fn load_model_settings(&self) -> StoreFuture<'_, assistant_protocol::ModelSettings> {
        VolatileRuntimeStore::load_model_settings(self)
    }

    fn save_model_settings(
        &self,
        settings: assistant_protocol::ModelSettings,
        server_history_key: Option<String>,
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::save_model_settings(self, settings, server_history_key)
    }

    fn invalidate_server_history_bindings(&self, key: String) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::invalidate_server_history_bindings(self, key)
    }

    fn get_model_fixed_config(
        &self,
        selection: assistant_protocol::ModelSelection,
    ) -> StoreFuture<'_, Option<assistant_protocol::ModelFixedConfig>> {
        VolatileRuntimeStore::get_model_fixed_config(self, selection)
    }

    fn list_model_fixed_configs(
        &self,
        id: assistant_protocol::ProviderInstanceId,
        offset: u32,
        limit: u32,
    ) -> StoreFuture<'_, Vec<assistant_protocol::ModelFixedConfig>> {
        VolatileRuntimeStore::list_model_fixed_configs(self, id, offset, limit)
    }

    fn put_model_fixed_config(
        &self,
        config: assistant_protocol::ModelFixedConfig,
        server_history_key: Option<crate::FixedModelHistoryInvalidation>,
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::put_model_fixed_config(self, config, server_history_key)
    }

    fn reset_model_fixed_config(
        &self,
        selection: assistant_protocol::ModelSelection,
        server_history_key: Option<crate::FixedModelHistoryInvalidation>,
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::reset_model_fixed_config(self, selection, server_history_key)
    }

    fn search_conversation_titles(
        &self,
        request: ConversationSearchRequest,
    ) -> StoreFuture<'_, ConversationSearchPage<assistant_protocol::ConversationHistoryHit>> {
        VolatileRuntimeStore::search_conversation_titles(self, request)
    }

    fn load_runtime_globals(&self) -> StoreFuture<'_, RecoveredRuntime> {
        VolatileRuntimeStore::load_runtime_globals(self)
    }

    fn prepare_session_execution(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, crate::LoadedSession> {
        VolatileRuntimeStore::prepare_session_execution(self, session_id)
    }

    fn load_session_environment(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, crate::SessionExecutionEnvironment> {
        VolatileRuntimeStore::load_session_environment(self, session_id)
    }

    fn load_session_state(&self, session_id: &SessionId) -> StoreFuture<'_, crate::LoadedSession> {
        VolatileRuntimeStore::load_session_state(self, session_id)
    }

    fn move_sidebar_item(
        &self,
        request: assistant_protocol::MoveSidebarItemRequest,
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::move_sidebar_item(self, request)
    }

    fn reset_sidebar_order(
        &self,
        request: assistant_protocol::ResetSidebarOrderRequest,
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::reset_sidebar_order(self, request)
    }

    fn query_workspaces(
        &self,
        request: assistant_protocol::ListWorkspacesRequest,
    ) -> StoreFuture<'_, crate::WorkspacePage> {
        VolatileRuntimeStore::query_workspaces(self, request)
    }

    fn query_sidebar(
        &self,
        request: assistant_protocol::GetApplicationSnapshotRequest,
    ) -> StoreFuture<'_, crate::SidebarSnapshot> {
        VolatileRuntimeStore::query_sidebar(self, request)
    }

    fn query_session_summaries(
        &self,
        query: crate::SessionSummaryQuery,
    ) -> StoreFuture<'_, Vec<assistant_protocol::SessionSummary>> {
        VolatileRuntimeStore::query_session_summaries(self, query)
    }

    fn load_runtime(&self) -> StoreFuture<'_, RecoveredRuntime> {
        VolatileRuntimeStore::load_runtime(self)
    }

    fn register_paired_device(&self, device: NewPairedDevice) -> StoreFuture<'_, PairedDevice> {
        VolatileRuntimeStore::register_paired_device(self, device)
    }

    fn rename_device(&self, change: DeviceNameChange) -> StoreFuture<'_, PairedDevice> {
        VolatileRuntimeStore::rename_device(self, change)
    }

    fn revoke_device(&self, change: DeviceRevocation) -> StoreFuture<'_, DeviceRevocationResult> {
        VolatileRuntimeStore::revoke_device(self, change)
    }

    fn set_pc_output_hosting(&self, change: PcOutputHostingChange) -> StoreFuture<'_, bool> {
        VolatileRuntimeStore::set_pc_output_hosting(self, change)
    }

    fn list_skill_name_states(&self) -> StoreFuture<'_, Vec<SkillNameState>> {
        VolatileRuntimeStore::list_skill_name_states(self)
    }

    fn set_skill_enabled(&self, change: SkillNameStateChange) -> StoreFuture<'_, SkillNameState> {
        VolatileRuntimeStore::set_skill_enabled(self, change)
    }

    fn list_custom_agents(&self) -> StoreFuture<'_, Vec<StoredCustomAgent>> {
        VolatileRuntimeStore::list_custom_agents(self)
    }

    fn mutate_custom_agent(
        &self,
        mutation: CustomAgentMutation,
    ) -> StoreFuture<'_, Option<StoredCustomAgent>> {
        VolatileRuntimeStore::mutate_custom_agent(self, mutation)
    }

    fn list_plugins(&self) -> StoreFuture<'_, Vec<StoredPlugin>> {
        VolatileRuntimeStore::list_plugins(self)
    }
    fn change_plugin_settings(
        &self,
        change: PluginSettingsChange,
    ) -> StoreFuture<'_, StoredPlugin> {
        VolatileRuntimeStore::change_plugin_settings(self, change)
    }

    fn list_plugin_operations(&self) -> StoreFuture<'_, Vec<StoredPluginOperation>> {
        VolatileRuntimeStore::list_plugin_operations(self)
    }

    fn accept_plugin_operation(
        &self,
        operation: StoredPluginOperation,
    ) -> StoreFuture<'_, StoredPluginOperation> {
        VolatileRuntimeStore::accept_plugin_operation(self, operation)
    }

    fn advance_plugin_operation(
        &self,
        update: PluginOperationUpdate,
    ) -> StoreFuture<'_, StoredPluginOperation> {
        VolatileRuntimeStore::advance_plugin_operation(self, update)
    }

    fn load_work_plan(&self, session_id: &SessionId) -> StoreFuture<'_, Option<StoredWorkPlan>> {
        VolatileRuntimeStore::load_work_plan(self, session_id)
    }

    fn mutate_work_plan(
        &self,
        mutation: WorkPlanMutation,
    ) -> StoreFuture<'_, WorkPlanMutationResult> {
        VolatileRuntimeStore::mutate_work_plan(self, mutation)
    }

    fn clear_work_plan(&self, clear: WorkPlanClear) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::clear_work_plan(self, clear)
    }

    fn load_memory_context(&self) -> StoreFuture<'_, MemoryContextSnapshot> {
        VolatileRuntimeStore::load_memory_context(self)
    }

    fn get_persona(&self) -> StoreFuture<'_, PersonaSnapshot> {
        VolatileRuntimeStore::get_persona(self)
    }

    fn set_persona(&self, mutation: PersonaMutation) -> StoreFuture<'_, PersonaSnapshot> {
        VolatileRuntimeStore::set_persona(self, mutation)
    }

    fn list_pinned_memories(&self) -> StoreFuture<'_, Vec<StoredPinnedMemory>> {
        VolatileRuntimeStore::list_pinned_memories(self)
    }

    fn mutate_pinned_memory(
        &self,
        mutation: PinnedMemoryMutation,
    ) -> StoreFuture<'_, PinnedMemoryMutationResult> {
        VolatileRuntimeStore::mutate_pinned_memory(self, mutation)
    }

    fn register_workspace(
        &self,
        registration: NewWorkspaceRegistration,
    ) -> StoreFuture<'_, StoredWorkspace> {
        VolatileRuntimeStore::register_workspace(self, registration)
    }

    fn update_workspace(&self, update: WorkspaceUpdate) -> StoreFuture<'_, StoredWorkspace> {
        VolatileRuntimeStore::update_workspace(self, update)
    }

    fn remove_workspace(&self, removal: WorkspaceRemoval) -> StoreFuture<'_, StoredWorkspace> {
        VolatileRuntimeStore::remove_workspace(self, removal)
    }

    fn restore_workspace(
        &self,
        restoration: WorkspaceRestoration,
    ) -> StoreFuture<'_, StoredWorkspace> {
        VolatileRuntimeStore::restore_workspace(self, restoration)
    }

    fn upload_attachment(&self, upload: NewAttachmentUpload) -> StoreFuture<'_, StoredAttachment> {
        VolatileRuntimeStore::upload_attachment(self, upload)
    }

    fn claim_input(
        &self,
        run: StoredRun,
        goal_resume: Option<GoalHeldInputResume>,
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::claim_input(self, run, goal_resume)
    }

    fn new_conversation_identity_available(
        &self,
        session: &SessionId,
        identity: crate::NewConversationIdentity,
    ) -> StoreFuture<'_, bool> {
        let session = session.clone();
        Box::pin(async move {
            let state = self.lock()?;
            if !state.is_history_available(&session) {
                return Err(StoreError::new(
                    StoreErrorKind::InvalidData,
                    "conversation history is unavailable",
                ));
            }
            Ok(match identity {
                crate::NewConversationIdentity::Input {
                    input_id,
                    message_id,
                } => state
                    .ensure_new_input_identity(&session, &input_id, None, &message_id)
                    .is_ok(),
                crate::NewConversationIdentity::Run(run) => {
                    state.ensure_new_run_identity(&session, &run).is_ok()
                }
            })
        })
    }

    fn load_run_conversation(
        &self,
        session: &SessionId,
        run: &RunId,
    ) -> StoreFuture<'_, crate::StoredConversationRawWindow> {
        let session = session.clone();
        let run = run.clone();
        Box::pin(async move {
            let state = self.lock()?;
            let history = state
                .histories
                .get(&session)
                .ok_or_else(|| conflict("Run history is missing"))?;
            let body = state
                .conversations
                .get(&session)
                .ok_or_else(|| conflict("Run conversation is missing"))?;
            let origins: Vec<_> = history
                .message_origins
                .iter()
                .filter(|origin| !origin.inherited && origin.run_id.as_ref() == Some(&run))
                .cloned()
                .collect();
            let owned: BTreeSet<_> = origins.iter().map(|origin| &origin.message_id).collect();
            let mut positions = body
                .messages
                .iter()
                .enumerate()
                .filter_map(|(index, message)| {
                    owned.contains(message_id(message)).then_some(index)
                });
            let (start, end) = positions.next().map_or((0, 0), |first| {
                (first, positions.next_back().unwrap_or(first) + 1)
            });
            Ok(crate::StoredConversationRawWindow {
                generation: state
                    .sessions
                    .get(&session)
                    .ok_or_else(|| conflict("session is missing"))?
                    .body_generation,
                start,
                end,
                total: body.messages.len(),
                conversation: ConversationSnapshot::new(body.messages[start..end].to_vec()),
                history: Some(crate::StoredHistoryFacts {
                    message_origins: origins,
                    ..Default::default()
                }),
            })
        })
    }

    fn find_request_acceptance(
        &self,
        session: &SessionId,
        key: &IdempotencyKey,
    ) -> StoreFuture<'_, Option<crate::StoredRequestAcceptance>> {
        let session = session.clone();
        let key = key.clone();
        Box::pin(async move {
            let state = self.lock()?;
            let mut result = None;
            if let Some(id) = state.message_request(&session, &key) {
                let input = state.input_projection(&session, id)?;
                result = Some(crate::StoredRequestAcceptance::Message(Box::new(
                    state.existing_input_acceptance(input)?,
                )));
            }
            if let Some(id) = state.command_request(&session, &key) {
                if result.is_some() {
                    return Err(conflict("request key belongs to both message and command"));
                }
                result = Some(crate::StoredRequestAcceptance::Command(Box::new(
                    state.command_projection(&session, id)?,
                )));
            }
            Ok(result)
        })
    }

    fn accept_input(&self, input: NewStoredInput) -> StoreFuture<'_, AcceptedInput> {
        VolatileRuntimeStore::accept_input(self, input)
    }

    fn accept_session_command(
        &self,
        command: NewStoredSessionCommand,
    ) -> StoreFuture<'_, AcceptedStoredSessionCommand> {
        VolatileRuntimeStore::accept_session_command(self, command)
    }

    fn cancel_queued_input(
        &self,
        session_id: &SessionId,
        input_id: &InputId,
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::cancel_queued_input(self, session_id, input_id)
    }

    fn prioritize_queued_input(&self, change: QueuePriorityChange) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::prioritize_queued_input(self, change)
    }

    fn create_run_attempt(&self, attempt: NewStoredRunAttempt) -> StoreFuture<'_, StoredRun> {
        VolatileRuntimeStore::create_run_attempt(self, attempt)
    }

    fn create_session(&self, session: NewStoredSession) -> StoreFuture<'_, StoredSession> {
        VolatileRuntimeStore::create_session(self, session)
    }

    fn materialize_session(
        &self,
        materialization: NewStoredSessionMaterialization,
    ) -> StoreFuture<'_, StoredSessionMaterialization> {
        VolatileRuntimeStore::materialize_session(self, materialization)
    }

    fn get_session_materialization(
        &self,
        key: &assistant_protocol::IdempotencyKey,
    ) -> StoreFuture<'_, Option<StoredSessionMaterialization>> {
        VolatileRuntimeStore::get_session_materialization(self, key)
    }

    fn fork_session(&self, fork: SessionFork) -> StoreFuture<'_, StoredSessionFork> {
        VolatileRuntimeStore::fork_session(self, fork)
    }

    fn inspect_session_deletion(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, assistant_protocol::DeleteSessionImpact> {
        VolatileRuntimeStore::inspect_session_deletion(self, session_id)
    }

    fn delete_session(&self, deletion: SessionDeletion) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::delete_session(self, deletion)
    }

    fn clear_session_history(
        &self,
        clear: SessionHistoryClear,
    ) -> StoreFuture<'_, SessionHistoryClearResult> {
        VolatileRuntimeStore::clear_session_history(self, clear)
    }

    fn prepare_session_compaction(
        &self,
        preparation: SessionHistoryCompactionPreparation,
    ) -> StoreFuture<'_, SessionHistoryCompactionPreparationResult> {
        VolatileRuntimeStore::prepare_session_compaction(self, preparation)
    }

    fn finish_session_compaction(
        &self,
        finish: SessionHistoryCompactionFinish,
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::finish_session_compaction(self, finish)
    }

    fn create_child_task(&self, task: NewStoredChildTask) -> StoreFuture<'_, StoredChildTask> {
        VolatileRuntimeStore::create_child_task(self, task)
    }

    fn start_child_task(&self, start: ChildTaskStart) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::start_child_task(self, start)
    }

    fn begin_child_tool_exchange(&self, pending: PendingChildToolExchange) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::begin_child_tool_exchange(self, pending)
    }

    fn mark_child_tool_execution_started(
        &self,
        start: ChildToolExecutionStart,
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::mark_child_tool_execution_started(self, start)
    }

    fn complete_child_tool_exchange(
        &self,
        completed: CompletedChildToolExchange,
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::complete_child_tool_exchange(self, completed)
    }

    fn settle_child_task(&self, settlement: StoredChildTaskSettlement) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::settle_child_task(self, settlement)
    }

    fn request_child_task_cancellation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StoreFuture<'_, StoredChildTask> {
        VolatileRuntimeStore::request_child_task_cancellation(self, session_id, child_task_id)
    }

    fn load_child_conversation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StoreFuture<'_, ConversationSnapshot> {
        VolatileRuntimeStore::load_child_conversation(self, session_id, child_task_id)
    }

    fn replace_context(
        &self,
        replacement: ContextReplacement,
    ) -> StoreFuture<'_, ContextReplacementResult> {
        VolatileRuntimeStore::replace_context(self, replacement)
    }

    fn commit_user_message(&self, commit: UserMessageCommit) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::commit_user_message(self, commit)
    }

    fn commit_session_command(
        &self,
        commit: SessionCommandCommit,
    ) -> StoreFuture<'_, StoredSessionCommand> {
        VolatileRuntimeStore::commit_session_command(self, commit)
    }

    fn begin_tool_exchange(&self, pending: PendingToolExchange) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::begin_tool_exchange(self, pending)
    }

    fn mark_tool_execution_started(&self, start: ToolExecutionStart) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::mark_tool_execution_started(self, start)
    }

    fn complete_tool_exchange(&self, completed: CompletedToolExchange) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::complete_tool_exchange(self, completed)
    }

    fn settle_run(
        &self,
        settlement: StoredRunSettlement,
    ) -> StoreFuture<'_, StoredRunSettlementResult> {
        VolatileRuntimeStore::settle_run(self, settlement)
    }

    fn reconcile_terminal_run_input(
        &self,
        reconciliation: StoredTerminalRunInputReconciliation,
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::reconcile_terminal_run_input(self, reconciliation)
    }

    fn commit_run_continuation(
        &self,
        continuation: StoredRunContinuation,
    ) -> StoreFuture<'_, StoredRunContinuationResult> {
        VolatileRuntimeStore::commit_run_continuation(self, continuation)
    }

    fn stop_goal(&self, stop: GoalStop) -> StoreFuture<'_, GoalStopResult> {
        VolatileRuntimeStore::stop_goal(self, stop)
    }

    fn clear_goal(&self, clear: GoalClear) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::clear_goal(self, clear)
    }

    fn resume_goal_with_held_input(
        &self,
        resume: GoalHeldInputResume,
    ) -> StoreFuture<'_, GoalHeldInputResumeResult> {
        VolatileRuntimeStore::resume_goal_with_held_input(self, resume)
    }

    fn load_conversation(&self, session_id: &SessionId) -> StoreFuture<'_, ConversationSnapshot> {
        VolatileRuntimeStore::load_conversation(self, session_id)
    }

    fn get_session_usage(&self, session_id: &SessionId) -> StoreFuture<'_, StoredSessionUsage> {
        VolatileRuntimeStore::get_session_usage(self, session_id)
    }

    fn load_conversation_window(
        &self,
        request: ConversationWindowRequest,
    ) -> StoreFuture<'_, StoredConversationWindow> {
        VolatileRuntimeStore::load_conversation_window(self, request)
    }

    fn read_conversation_navigation(
        &self,
        request: ConversationNavigationRequest,
    ) -> StoreFuture<'_, StoredConversationNavigation> {
        VolatileRuntimeStore::read_conversation_navigation(self, request)
    }

    fn load_conversation_raw_window(
        &self,
        request: ConversationRawWindowRequest,
    ) -> StoreFuture<'_, StoredConversationRawWindow> {
        VolatileRuntimeStore::load_conversation_raw_window(self, request)
    }

    fn locate_conversation_message(
        &self,
        request: ConversationMessageLocationRequest,
    ) -> StoreFuture<'_, Option<StoredConversationMessageLocation>> {
        VolatileRuntimeStore::locate_conversation_message(self, request)
    }

    fn search_conversations(
        &self,
        request: ConversationSearchRequest,
    ) -> StoreFuture<'_, ConversationSearchPage> {
        VolatileRuntimeStore::search_conversations(self, request)
    }

    fn set_session_archive(&self, change: ArchiveChange) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::set_session_archive(self, change)
    }

    fn set_session_proxy(&self, change: SessionProxyChange) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::set_session_proxy(self, change)
    }

    fn rename_session(&self, change: SessionTitleChange) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::rename_session(self, change)
    }

    fn disable_automatic_title(&self, session_id: &SessionId) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::disable_automatic_title(self, session_id)
    }

    fn commit_session_title_generation(
        &self,
        commit: SessionTitleGenerationCommit,
    ) -> StoreFuture<'_, SessionTitleGenerationCommitResult> {
        VolatileRuntimeStore::commit_session_title_generation(self, commit)
    }

    fn set_session_pinned(&self, change: SessionPinnedChange) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::set_session_pinned(self, change)
    }

    fn set_message_feedback(&self, change: MessageFeedbackChange) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::set_message_feedback(self, change)
    }

    fn load_message_feedback(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, Vec<StoredMessageFeedback>> {
        VolatileRuntimeStore::load_message_feedback(self, session_id)
    }

    fn set_session_model(&self, change: ModelChange) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::set_session_model(self, change)
    }

    fn set_session_reasoning_effort(&self, change: ReasoningEffortChange) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::set_session_reasoning_effort(self, change)
    }

    fn set_session_variant(&self, change: VariantChange) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::set_session_variant(self, change)
    }

    fn set_session_approval_mode(&self, change: ApprovalModeChange) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::set_session_approval_mode(self, change)
    }

    fn rewrite_from_user(&self, rewrite: ConversationRewrite) -> StoreFuture<'_, RewriteResult> {
        VolatileRuntimeStore::rewrite_from_user(self, rewrite)
    }

    fn shutdown(&self) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::shutdown(self)
    }
}
