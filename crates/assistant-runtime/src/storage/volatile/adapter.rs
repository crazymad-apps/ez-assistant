use super::*;

impl RuntimeStore for VolatileRuntimeStore {
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
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::save_model_settings(self, settings)
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
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::put_model_fixed_config(self, config)
    }

    fn reset_model_fixed_config(
        &self,
        selection: assistant_protocol::ModelSelection,
    ) -> StoreFuture<'_, ()> {
        VolatileRuntimeStore::reset_model_fixed_config(self, selection)
    }

    fn search_conversation_titles(
        &self,
        request: ConversationSearchRequest,
    ) -> StoreFuture<'_, Vec<assistant_protocol::ConversationHistoryHit>> {
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

    fn upload_attachment(&self, upload: NewAttachmentUpload) -> StoreFuture<'_, StoredAttachment> {
        VolatileRuntimeStore::upload_attachment(self, upload)
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
