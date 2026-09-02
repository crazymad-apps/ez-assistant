//! 专用阻塞线程上的 StorageEngine 命令分发。

use std::path::PathBuf;

use assistant_runtime::{StoreError, StoreErrorKind};
use tokio::sync::{mpsc, oneshot};

use super::super::StorageEngine;
use super::command::Command;

pub(super) fn run_worker(
    runtime_home: PathBuf,
    mut receiver: mpsc::Receiver<Command>,
    ready: oneshot::Sender<Result<(), StoreError>>,
) {
    let mut engine = match StorageEngine::open(&runtime_home) {
        Ok(engine) => {
            let _ = ready.send(Ok(()));
            engine
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };

    while let Some(command) = receiver.blocking_recv() {
        match command {
            #[cfg(test)]
            Command::PanicForTest { reply } => {
                drop(reply);
                panic!("private storage worker panic payload");
            }
            Command::LoadRuntime { reply } => {
                let _ = reply.send(engine.load_runtime());
            }
            Command::RegisterPairedDevice { device, reply } => {
                let _ = reply.send(engine.register_paired_device(device));
            }
            Command::RenameDevice { change, reply } => {
                let _ = reply.send(engine.rename_device(change));
            }
            Command::RevokeDevice { change, reply } => {
                let _ = reply.send(engine.revoke_device(change));
            }
            Command::SetPcOutputHosting { change, reply } => {
                let _ = reply.send(engine.set_pc_output_hosting(change));
            }
            Command::LoadMemoryContext { reply } => {
                let _ = reply.send(engine.load_memory_context());
            }
            Command::ListSkillNameStates { reply } => {
                let _ = reply.send(engine.list_skill_name_states());
            }
            Command::SetSkillEnabled { change, reply } => {
                let _ = reply.send(engine.set_skill_enabled(change));
            }
            Command::LoadWorkPlan { session_id, reply } => {
                let _ = reply.send(engine.load_work_plan(&session_id));
            }
            Command::MutateWorkPlan { mutation, reply } => {
                let _ = reply.send(engine.mutate_work_plan(mutation));
            }
            Command::ClearWorkPlan { clear, reply } => {
                let _ = reply.send(engine.clear_work_plan(clear));
            }
            Command::GetPersona { reply } => {
                let _ = reply.send(engine.get_persona());
            }
            Command::SetPersona { mutation, reply } => {
                let _ = reply.send(engine.set_persona(mutation));
            }
            Command::ListPinnedMemories { reply } => {
                let _ = reply.send(engine.list_pinned_memories());
            }
            Command::MutatePinnedMemory { mutation, reply } => {
                let _ = reply.send(engine.mutate_pinned_memory(mutation));
            }
            Command::RegisterWorkspace {
                registration,
                reply,
            } => {
                let _ = reply.send(engine.register_workspace(registration));
            }
            Command::UpdateWorkspace { update, reply } => {
                let _ = reply.send(engine.update_workspace(update));
            }
            Command::RemoveWorkspace { removal, reply } => {
                let _ = reply.send(engine.remove_workspace(removal));
            }
            Command::UploadAttachment { upload, reply } => {
                let _ = reply.send(engine.upload_attachment(upload));
            }
            Command::CreateSession { session, reply } => {
                let _ = reply.send(engine.create_session(session));
            }
            Command::MaterializeSession {
                materialization,
                reply,
            } => {
                let _ = reply.send(engine.materialize_session(*materialization));
            }
            Command::ForkSession { fork, reply } => {
                let _ = reply.send(engine.fork_session(*fork));
            }
            Command::InspectSessionDeletion { session_id, reply } => {
                let _ = reply.send(engine.inspect_session_deletion(&session_id));
            }
            Command::DeleteSession { deletion, reply } => {
                let _ = reply.send(engine.delete_session(deletion));
            }
            Command::ClearSessionHistory { clear, reply } => {
                let _ = reply.send(engine.clear_session_history(*clear));
            }
            Command::PrepareSessionCompaction { preparation, reply } => {
                let _ = reply.send(engine.prepare_session_compaction(preparation));
            }
            Command::FinishSessionCompaction { finish, reply } => {
                let _ = reply.send(engine.finish_session_compaction(finish));
            }
            Command::CreateChildTask { task, reply } => {
                let _ = reply.send(engine.create_child_task(task));
            }
            Command::StartChildTask { start, reply } => {
                let _ = reply.send(engine.start_child_task(start));
            }
            Command::BeginChildToolExchange { pending, reply } => {
                let _ = reply.send(engine.begin_child_tool_exchange(pending));
            }
            Command::MarkChildToolExecutionStarted { start, reply } => {
                let _ = reply.send(engine.mark_child_tool_execution_started(start));
            }
            Command::CompleteChildToolExchange { completed, reply } => {
                let _ = reply.send(engine.complete_child_tool_exchange(completed));
            }
            Command::SettleChildTask { settlement, reply } => {
                let _ = reply.send(engine.settle_child_task(settlement));
            }
            Command::RequestChildTaskCancellation {
                session_id,
                child_task_id,
                reply,
            } => {
                let _ =
                    reply.send(engine.request_child_task_cancellation(&session_id, &child_task_id));
            }
            Command::LoadChildConversation {
                session_id,
                child_task_id,
                reply,
            } => {
                let _ = reply.send(engine.load_child_conversation(&session_id, &child_task_id));
            }
            Command::ReplaceContext { replacement, reply } => {
                let _ = reply.send(engine.replace_context(replacement));
            }
            Command::AcceptInput { input, reply } => {
                let _ = reply.send(engine.accept_input(*input));
            }
            Command::CancelQueuedInput {
                session_id,
                input_id,
                reply,
            } => {
                let _ = reply.send(engine.cancel_queued_input(&session_id, &input_id));
            }
            Command::PrioritizeQueuedInput { change, reply } => {
                let _ = reply.send(engine.prioritize_queued_input(change));
            }
            Command::CreateRunAttempt { attempt, reply } => {
                let _ = reply.send(engine.create_run_attempt(attempt));
            }
            Command::CommitUserMessage { commit, reply } => {
                let _ = reply.send(engine.commit_user_message(commit));
            }
            Command::BeginToolExchange { pending, reply } => {
                let _ = reply.send(engine.begin_tool_exchange(pending));
            }
            Command::MarkToolExecutionStarted { start, reply } => {
                let _ = reply.send(engine.mark_tool_execution_started(start));
            }
            Command::CompleteToolExchange { completed, reply } => {
                let _ = reply.send(engine.complete_tool_exchange(completed));
            }
            Command::CommitRunContinuation {
                continuation,
                reply,
            } => {
                let _ = reply.send(engine.commit_run_continuation(*continuation));
            }
            Command::SettleRun { settlement, reply } => {
                let _ = reply.send(engine.settle_run(*settlement));
            }
            Command::StopGoal { stop, reply } => {
                let _ = reply.send(engine.stop_goal(stop));
            }
            Command::ClearGoal { clear, reply } => {
                let _ = reply.send(engine.clear_goal(clear));
            }
            Command::ResumeGoalWithHeldInput { resume, reply } => {
                let _ = reply.send(engine.resume_goal_with_held_input(resume));
            }
            Command::LoadConversation { session_id, reply } => {
                let _ = reply.send(engine.load_conversation(&session_id));
            }
            Command::GetSessionUsage { session_id, reply } => {
                let _ = reply.send(engine.get_session_usage(&session_id));
            }
            Command::LoadConversationWindow { request, reply } => {
                let _ = reply.send(engine.load_conversation_window(request));
            }
            Command::LoadConversationRawWindow { request, reply } => {
                let _ = reply.send(engine.load_conversation_raw_window(request));
            }
            Command::LocateConversationMessage { request, reply } => {
                let _ = reply.send(engine.locate_conversation_message(request));
            }
            Command::SearchConversations { request, reply } => {
                let _ = reply.send(engine.search_conversations(request));
            }
            Command::SetSessionArchive { change, reply } => {
                let _ = reply.send(engine.set_session_archive(change));
            }
            Command::SetSessionProxy { change, reply } => {
                let _ = reply.send(engine.set_session_proxy(change));
            }
            Command::RenameSession { change, reply } => {
                let _ = reply.send(engine.rename_session(change));
            }
            Command::DisableAutomaticTitle { session_id, reply } => {
                let _ = reply.send(engine.disable_automatic_title(&session_id));
            }
            Command::CommitSessionTitleGeneration { commit, reply } => {
                let _ = reply.send(engine.commit_session_title_generation(commit));
            }
            Command::SetSessionPinned { change, reply } => {
                let _ = reply.send(engine.set_session_pinned(change));
            }
            Command::SetMessageFeedback { change, reply } => {
                let _ = reply.send(engine.set_message_feedback(change));
            }
            Command::LoadMessageFeedback { session_id, reply } => {
                let _ = reply.send(engine.load_message_feedback(&session_id));
            }
            Command::SetSessionModel { change, reply } => {
                let _ = reply.send(engine.set_session_model(change));
            }
            Command::SetSessionReasoningEffort { change, reply } => {
                let _ = reply.send(engine.set_session_reasoning_effort(change));
            }
            Command::SetSessionVariant { change, reply } => {
                let _ = reply.send(engine.set_session_variant(change));
            }
            Command::SetSessionApprovalMode { change, reply } => {
                let _ = reply.send(engine.set_session_approval_mode(change));
            }
            Command::RewriteFromUser { rewrite, reply } => {
                let _ = reply.send(engine.rewrite_from_user(*rewrite));
            }
            Command::LoadPermissionFile { scope, reply } => {
                let _ = reply.send(engine.load_permission_file(&scope));
            }
            Command::ReplacePermissionFile {
                scope,
                expected_revision,
                content,
                reply,
            } => {
                let _ = reply.send(engine.replace_permission_file(
                    &scope,
                    &expected_revision,
                    &content,
                ));
            }
            Command::Shutdown { reply } => {
                let _ = reply.send(Ok(()));
                break;
            }
        }
    }
}

pub(super) fn worker_unavailable() -> StoreError {
    StoreError::new(
        StoreErrorKind::Unavailable,
        "runtime storage worker is unavailable",
    )
}
