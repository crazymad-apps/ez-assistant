//! Session Fork 与永久删除的跨 SQLite/JSONL/Attachment 介质提交边界。

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use agent_types::{ConversationMessage, ConversationSnapshot, UserPart};
use assistant_protocol::{ConversationOwner, DeleteSessionImpact, SessionId};
use assistant_runtime::{
    SessionDeletion, SessionFork, StoreError, StoreErrorKind, StoredAttachment,
    StoredAttachmentState, StoredConversationState, StoredSession, StoredSessionFork,
    StoredSessionLifecycle, StoredWorkPlan,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, attachment_io, body_path, conflict, conversation,
    database_write_error,
    goal::insert_forked_goal,
    internal_error, invalid_data,
    mode::{agent_variant_value, approval_mode_value, reasoning_effort_value},
    session_resources::remove_created_session_directories,
    sync_directory, to_i64,
};

impl StorageEngine {
    pub(super) fn fork_session(&mut self, fork: SessionFork) -> StorageResult<StoredSessionFork> {
        self.ensure_no_pending_history(&ConversationOwner::MainSession {
            session_id: fork.source_session_id.clone(),
        })?;
        super::filesystem::validate_session_component(&fork.source_session_id)?;
        super::filesystem::validate_session_component(&fork.session.session_id)?;
        fork.conversation
            .validate_tool_exchange_pairs()
            .map_err(|source| {
                assistant_runtime::StoreError::with_source(
                    StoreErrorKind::InvalidInput,
                    "fork conversation splits a tool exchange",
                    source,
                )
            })?;
        let (source_generation, source_role) = self
            .connection
            .query_row(
                "SELECT body_generation, role FROM sessions WHERE session_id = ?1",
                [fork.source_session_id.as_str()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|source| {
                internal_error("fork source generation could not be queried", source)
            })?
            .ok_or_else(|| conflict("fork source session does not exist"))?;
        if u64::try_from(source_generation).ok() != Some(fork.source_generation) {
            return Err(conflict("fork source generation changed"));
        }
        if source_role != "standard"
            || fork.session.role != assistant_runtime::SessionRole::Standard
        {
            return Err(conflict("session role cannot be forked"));
        }
        // 当前 worker 操作内核对同一源代次，裁切只读事实；不创建任何继承执行控制。
        let source_history = self.load_conversation_history(ConversationOwner::MainSession {
            session_id: fork.source_session_id.clone(),
        })?;
        if source_history.generation != fork.source_generation {
            return Err(conflict("fork source history generation changed"));
        }
        let mut inherited = source_history.facts.fork_lineage(
            &fork.session.session_id,
            &fork.conversation,
            &fork.session_commands,
        )?;
        if fork
            .work_plan
            .as_ref()
            .is_some_and(|plan| plan.session_id != fork.source_session_id)
        {
            return Err(conflict("fork work plan belongs to a different session"));
        }
        if let Some(goal) = fork.goal.as_ref() {
            let source_goal = self
                .load_all_goals()?
                .into_iter()
                .find(|candidate| candidate.session_id == fork.source_session_id)
                .ok_or_else(|| conflict("fork source Goal does not exist"))?;
            if goal.session_id != fork.session.session_id
                || goal.goal_id == source_goal.goal_id
                || goal.objective != source_goal.objective
                || goal.turn != source_goal.turn
                || goal.budget != source_goal.budget
                || goal.consecutive_failures != source_goal.consecutive_failures
                || !fork.conversation.messages.iter().any(|message| {
                    matches!(message, ConversationMessage::User(user)
                        if user.id == goal.objective.source_message_id)
                })
            {
                return Err(conflict("fork Goal projection is invalid"));
            }
        }
        let message_ids = fork
            .conversation
            .messages
            .iter()
            .map(|message| conversation::message_id(message).as_str().to_owned())
            .collect::<BTreeSet<_>>();
        let mut activation_ids = BTreeSet::new();
        for activation in &fork.skill_activations {
            if !activation_ids.insert(activation.activation_id.clone())
                || activation.session_id != fork.session.session_id
                || !matches!(
                    &activation.owner,
                    assistant_runtime::SkillActivationOwner::Session(session_id)
                        if session_id == &fork.session.session_id
                )
                || activation.run_id.is_some()
                || activation.input_id.is_some()
                || !message_ids.contains(activation.message_id.as_str())
                || !activation.has_valid_definition_identity()
            {
                return Err(conflict("fork skill activation is invalid"));
            }
        }
        let stored_selections = &source_history.facts.mcp_selections;
        let mut selection_ids = BTreeSet::new();
        for selection in &fork.mcp_selections {
            let source_selection = stored_selections.iter().find(|candidate| {
                candidate.session_id == fork.source_session_id
                    && candidate.message_id == selection.message_id
            });
            if !selection_ids.insert(selection.selection_id.clone())
                || selection.session_id != fork.session.session_id
                || selection.input_id.is_some()
                || !message_ids.contains(selection.message_id.as_str())
                || !source_selection.is_some_and(|source| {
                    source.server_key == selection.server_key
                        && source.display_name == selection.display_name
                        && source.created_at_ms == selection.created_at_ms
                })
            {
                return Err(conflict("fork MCP selection is invalid"));
            }
        }
        let stored_commands = &source_history.facts.commands;
        let mut command_ids = BTreeSet::new();
        let mut command_message_ids = BTreeSet::new();
        for copied in &fork.session_commands {
            let command = &copied.command;
            let source = stored_commands
                .iter()
                .find(|source| source.input_id == copied.source_input_id);
            if !command_ids.insert(command.input_id.clone())
                || !command_message_ids.insert(command.user_message_id.clone())
                || stored_commands
                    .iter()
                    .any(|source| source.input_id == command.input_id)
                || command.session_id != fork.session.session_id
                || command.idempotency_key.is_some()
                || command.state != assistant_runtime::StoredSessionCommandState::Committed
                || !message_ids.contains(command.user_message_id.as_str())
                || !source.is_some_and(|source| {
                    source.session_id == fork.source_session_id
                        && source.state == assistant_runtime::StoredSessionCommandState::Committed
                        && source.user_message_id != command.user_message_id
                        && source.command == command.command
                        && source.result == command.result
                        && source.agent_variant == command.agent_variant
                        && source.accepted_at_ms == command.accepted_at_ms
                })
            {
                return Err(conflict("fork session command is invalid"));
            }
        }
        let work_plan = fork.work_plan.as_ref().map(|source| StoredWorkPlan {
            session_id: fork.session.session_id.clone(),
            revision: 1,
            objective: source.objective.clone(),
            items: source.items.clone(),
            last_operation_id: format!("fork:{}", fork.source_session_id),
            updated_at_ms: fork.session.created_at_ms,
        });
        let work_plan_items_json = work_plan
            .as_ref()
            .map(|plan| serde_json::to_string(&plan.items))
            .transpose()
            .map_err(|source| {
                internal_error("fork work plan items could not be encoded", source)
            })?;
        let paths =
            self.prepare_fork_session_directories(&fork.source_session_id, &fork.session)?;
        let prepared = (|| -> StorageResult<_> {
            let source_attachments = self
                .load_attachments()?
                .into_iter()
                .filter(|attachment| attachment.session_id == fork.source_session_id)
                .map(|attachment| (attachment.attachment_id.clone(), attachment))
                .collect::<BTreeMap<_, _>>();
            let mut rewrites = BTreeMap::new();
            let mut attachments = Vec::with_capacity(fork.attachments.len());
            for reference in &fork.attachments {
                super::filesystem::validate_attachment_component(&reference.attachment_id)?;
                let source = source_attachments
                    .get(&reference.source_attachment_id)
                    .ok_or_else(|| conflict("fork attachment does not belong to source session"))?;
                let view = attachment_io::stable_view_path(
                    &paths.attachment_directory,
                    &reference.attachment_id,
                    &source.original_name,
                );
                attachment_io::ensure_stable_view(&view, &source.blob_hash, &source.original_name)?;
                let readable_path = path_text(&view)?;
                rewrites.insert(source.agent_readable_path.clone(), readable_path.clone());
                if let Some(old) = super::resource_reference::historical_attachment_path(
                    &self.runtime_home,
                    source,
                ) {
                    rewrites.insert(old, readable_path.clone());
                }
                attachments.push(StoredAttachment {
                    attachment_id: reference.attachment_id.clone(),
                    session_id: fork.session.session_id.clone(),
                    original_name: source.original_name.clone(),
                    blob_hash: source.blob_hash.clone(),
                    size_bytes: source.size_bytes,
                    media_type: source.media_type.clone(),
                    agent_readable_path: readable_path,
                    state: source.state,
                    created_at_ms: fork.session.created_at_ms,
                });
            }
            let source_tool_images = self
                .session_directory(&fork.source_session_id)?
                .join("tool-images");
            for reference in &fork.tool_images {
                crate::image::copy_tool_image(
                    &source_tool_images,
                    &paths.tool_image_directory,
                    reference,
                )
                .map_err(|source| {
                    StoreError::with_source(
                        StoreErrorKind::ResourceUnavailable,
                        "fork tool image could not be copied",
                        source,
                    )
                })?;
            }
            let mut forked_conversation = fork.conversation;
            rewrite_file_reference_paths(&mut forked_conversation, &rewrites)?;
            // child 的目录、正文和图片同属目标 Session；全部文件先准备，再由下方
            // 既有事务发布 Session。来源正文损坏仅保留明确不可用的可靠父描述。
            for child in &mut inherited.child_tasks {
                if child.body_unavailable {
                    continue;
                }
                super::filesystem::validate_child_task_component(&child.child_task_id)?;
                let source_child = source_history
                    .facts
                    .child_tasks
                    .iter()
                    .find(|source| source.child_task_id == child.child_task_id)
                    .ok_or_else(|| invalid_data("fork child source directory is missing"))?;
                let source_owner = ConversationOwner::ChildTask {
                    session_id: fork.source_session_id.clone(),
                    child_task_id: child.child_task_id.clone(),
                };
                self.ensure_no_pending_history(&source_owner)?;
                let source_body = self.child_body(
                    &fork.source_session_id,
                    &child.child_task_id,
                    source_child.body_generation,
                )?;
                let document = match self.read_owner_document(
                    &source_body,
                    &source_owner,
                    source_child.body_generation,
                ) {
                    Ok(document) => document,
                    Err(_) => {
                        child.body_unavailable = true;
                        continue;
                    }
                };
                let source_facts = document
                    .history
                    .ok_or_else(|| invalid_data("fork child history is missing"))?;
                if !source_facts
                    .child_tasks
                    .iter()
                    .any(|fact| fact == source_child)
                {
                    return Err(conflict("fork child source binding changed"));
                }
                let mut child_body = document.conversation;
                let mut child_facts =
                    source_facts.fork_lineage(&fork.session.session_id, &child_body, &[])?;
                if rewrite_file_reference_paths(&mut child_body, &rewrites).is_err() {
                    child.body_unavailable = true;
                    continue;
                }
                child_facts.attachments =
                    fork_attachment_history(&child_body, &attachments, &paths.session_directory)?;
                child_facts.child_tasks = vec![child.clone()];
                child_facts.skill_activations = source_facts
                    .skill_activations
                    .into_iter()
                    .map(|mut activation| {
                        activation.session_id = fork.session.session_id.clone();
                        activation.run_id = None;
                        activation.input_id = None;
                        activation
                    })
                    .collect();
                let mut images = BTreeSet::new();
                super::tool_images::collect_tool_images(&child_body, &mut images);
                for image in images {
                    // 丢失图片由目标图片端点明确报告 unavailable；不能为了继续分支
                    // 链接来源目录，也不能忽略已核实图片的目标写入失败。
                    if crate::image::validate_tool_image_file(&source_tool_images, &image).is_err()
                    {
                        continue;
                    }
                    crate::image::copy_tool_image(
                        &source_tool_images,
                        &paths.tool_image_directory,
                        &image,
                    )
                    .map_err(|source| {
                        StoreError::with_source(
                            StoreErrorKind::ResourceUnavailable,
                            "fork child image could not be copied",
                            source,
                        )
                    })?;
                }
                let child_directory =
                    super::child_task_directory(&paths.session_directory, &child.child_task_id);
                let tasks_directory = super::child_tasks_directory(&paths.session_directory);
                crate::config_source::prepare_private_directory(&tasks_directory)
                    .and_then(|()| {
                        crate::config_source::prepare_private_directory(&child_directory)
                    })
                    .map_err(|source| {
                        internal_error("fork child directory could not be prepared", source)
                    })?;
                conversation::write_generation(
                    &super::child_body_path(&child_directory, 1),
                    &ConversationOwner::ChildTask {
                        session_id: fork.session.session_id.clone(),
                        child_task_id: child.child_task_id.clone(),
                    },
                    1,
                    &child_body,
                    &child_facts,
                )?;
                sync_directory(&child_directory)?;
                sync_directory(&tasks_directory)?;
            }
            let body = body_path(&paths.session_directory, 1);
            let history = assistant_runtime::StoredHistoryFacts {
                commands: fork
                    .session_commands
                    .iter()
                    .map(|copied| copied.command.clone())
                    .collect(),
                skill_activations: fork.skill_activations.clone(),
                mcp_selections: fork.mcp_selections.clone(),
                attachments: fork_attachment_history(
                    &forked_conversation,
                    &attachments,
                    &paths.session_directory,
                )?,
                ..inherited
            };
            conversation::write_generation(
                &body,
                &ConversationOwner::MainSession {
                    session_id: fork.session.session_id.clone(),
                },
                1,
                &forked_conversation,
                &history,
            )?;
            sync_directory(&paths.session_directory)?;
            let prompt_json =
                serde_json::to_string(&fork.session.system_prompt).map_err(|source| {
                    internal_error("fork system prompt could not be encoded", source)
                })?;
            let skill_catalog_json = "{}";
            let message_count =
                u64::try_from(forked_conversation.messages.len()).map_err(|source| {
                    StoreError::with_source(
                        StoreErrorKind::InvalidInput,
                        "fork conversation is too large",
                        source,
                    )
                })?;
            Ok((
                forked_conversation,
                attachments,
                prompt_json,
                skill_catalog_json,
                message_count,
            ))
        })();
        let (forked_conversation, attachments, prompt_json, skill_catalog_json, message_count) =
            match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    remove_created_session_directories(&paths);
                    return Err(error);
                }
            };
        // Session 目录及其中的 Conversation/Attachment 视图必须先持久化，再提交
        // SQLite 中对这些文件的引用。提交成功后不再执行可能导致“已提交但返回失败”的步骤。
        sync_directory(&self.sessions_directory)?;
        let persisted = (|| -> StorageResult<()> {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| {
                    database_write_error("fork transaction could not begin", source)
                })?;
            transaction
                .execute(
                    "INSERT INTO sessions (
                        session_id, title, model_id, reasoning_effort, system_prompt_json, skill_catalog_json, current_variant,
                        approval_mode, role, lifecycle, body_generation, message_count, created_at_ms,
                        updated_at_ms, archived_at_ms, is_pinned, title_origin,
                        materialization_key, automatic_title_pending, model_provider_instance_id, agent_shell_kind, agent_shell_environment_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'active', 1, ?10, ?11, ?11, NULL, 0, ?12, ?13, ?14, ?15, ?16, ?17)",
                    params![
                        fork.session.session_id.as_str(),
                        fork.session.title,
                        fork.session.model_selection.as_ref().map(|selection| selection.model_id.as_str()),
                        fork.session.reasoning_effort.map(reasoning_effort_value),
                        prompt_json,
                        skill_catalog_json,
                        agent_variant_value(fork.session.current_variant),
                        approval_mode_value(fork.session.approval_mode),
                        match fork.session.role {
                            assistant_runtime::SessionRole::Standard => "standard",
                            assistant_runtime::SessionRole::Controller => "controller",
                        },
                        to_i64(message_count, "fork message count exceeds SQLite range")?,
                        fork.session.created_at_ms,
                        match fork.session.title_origin {
                            assistant_protocol::SessionTitleOrigin::Generated => "generated",
                            assistant_protocol::SessionTitleOrigin::User => "user",
                        },
                        fork.session
                            .materialization_key
                            .as_ref()
                            .map(|key| key.as_str()),
                        i64::from(fork.session.automatic_title_pending),
                        fork.session.model_selection.as_ref().map(|selection| selection.provider_instance_id.as_str()),
                        fork.session.agent_shell_kind.map(super::mode::shell_kind_value),
super::agent_shell::encode_environment(fork.session.agent_shell_environment.as_ref())?,
                    ],
                )
                .map_err(|source| {
                    database_write_error("fork session could not be created", source)
                })?;
            Self::insert_session_resources(&transaction, &fork.session)?;
            transaction
                .execute(
                    "INSERT INTO session_usage (session_id, backfilled, updated_at_ms)
                     VALUES (?1, 1, ?2)",
                    params![fork.session.session_id.as_str(), fork.session.created_at_ms],
                )
                .map_err(|source| {
                    database_write_error("fork session usage could not be initialized", source)
                })?;
            if let (Some(plan), Some(items_json)) = (&work_plan, &work_plan_items_json) {
                transaction
                    .execute(
                        "INSERT INTO session_work_plans (
                            session_id, revision, objective, items_json, last_operation_id,
                            updated_at_ms
                         ) VALUES (?1, 1, ?2, ?3, ?4, ?5)",
                        params![
                            plan.session_id.as_str(),
                            plan.objective,
                            items_json,
                            plan.last_operation_id,
                            plan.updated_at_ms,
                        ],
                    )
                    .map_err(|source| {
                        database_write_error("fork work plan could not be created", source)
                    })?;
            }
            if let Some(goal) = fork.goal.as_ref() {
                insert_forked_goal(&transaction, goal)?;
            }
            // 继承选择已经写入目标文件，不创建可执行队列或 SQL 历史副本。
            // 继承控制结果已在目标文件，不创建已完成命令的 SQL 身份或队列。
            for attachment in &attachments {
                transaction
                    .execute(
                        "INSERT INTO attachments (
                            attachment_id, session_id, blob_hash, original_name,
                            agent_readable_path, state, created_at_ms
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            attachment.attachment_id.as_str(),
                            attachment.session_id.as_str(),
                            attachment.blob_hash,
                            attachment.original_name,
                            attachment.agent_readable_path,
                            match attachment.state {
                                StoredAttachmentState::Ready => "ready",
                                StoredAttachmentState::Unavailable => "unavailable",
                            },
                            attachment.created_at_ms,
                        ],
                    )
                    .map_err(|source| {
                        database_write_error(
                            "fork attachment reference could not be created",
                            source,
                        )
                    })?;
            }
            transaction
                .commit()
                .map_err(|source| database_write_error("fork transaction could not commit", source))
        })();
        if let Err(error) = persisted {
            remove_created_session_directories(&paths);
            return Err(error);
        }
        let stored = StoredSessionFork {
            session: StoredSession {
                context_management_mode: None,
                server_history_key: None,
                custom_agent: fork.session.custom_agent,
                agent_shell_kind: fork.session.agent_shell_kind,
                agent_shell_environment: fork.session.agent_shell_environment.clone(),
                session_id: fork.session.session_id,
                title: fork.session.title,
                model_selection: fork.session.model_selection,
                reasoning_effort: fork.session.reasoning_effort,
                system_prompt: fork.session.system_prompt,

                environment: fork.session.environment,
                lifecycle: StoredSessionLifecycle::Active,
                current_variant: fork.session.current_variant,
                approval_mode: fork.session.approval_mode,
                role: fork.session.role,
                materialization_key: fork.session.materialization_key,
                automatic_title_pending: fork.session.automatic_title_pending,
                proxy: None,
                pc_output_hosting: None,
                body_generation: 1,
                message_count,
                created_at_ms: fork.session.created_at_ms,
                updated_at_ms: fork.session.created_at_ms,
                archived_at_ms: None,
                is_pinned: false,
                title_origin: fork.session.title_origin,
                conversation_state: StoredConversationState::Available,
            },
            conversation: forked_conversation,
            attachments,
            skill_activations: fork.skill_activations,
            mcp_selections: fork.mcp_selections,
            session_commands: fork
                .session_commands
                .into_iter()
                .map(|copied| copied.command)
                .collect(),
            work_plan,
            goal: fork.goal,
        };
        self.mark_recall_owner_dirty_now(
            &ConversationOwner::MainSession {
                session_id: stored.session.session_id.clone(),
            },
            1,
        );
        Ok(stored)
    }

    /// 删除确认读取可靠文件及当前控制／上传身份的并集。终态控制释放不减少历史影响数，
    /// 同一身份在两边出现只计一次；有待提交尾帧或文件损坏时不签发猜测的影响数量。
    pub(super) fn inspect_session_deletion(
        &mut self,
        session_id: &SessionId,
    ) -> StorageResult<DeleteSessionImpact> {
        super::filesystem::validate_session_component(session_id)?;
        let owner = ConversationOwner::MainSession {
            session_id: session_id.clone(),
        };
        self.ensure_no_pending_history(&owner)?;
        let generation = self.session_generation(session_id)?;
        let mut runs = std::collections::HashSet::new();
        let mut children = std::collections::HashSet::new();
        let mut attachments = std::collections::HashSet::new();
        {
            let mut statement = self.connection.prepare(
                "SELECT 'run',run_id FROM runs WHERE session_id=?1 AND status IN ('accepted','running','cancelling')
                 UNION ALL SELECT 'child',child_task_id FROM child_tasks WHERE session_id=?1 AND status IN ('accepted','running')
                 UNION ALL SELECT 'attachment',attachment_id FROM attachments WHERE session_id=?1",
            ).map_err(|source| internal_error("delete impact controls could not be queried",source))?;
            let rows = statement
                .query_map([session_id.as_str()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|source| {
                    internal_error("delete impact controls could not be read", source)
                })?;
            for row in rows {
                let (kind, id) = row.map_err(|source| {
                    internal_error("delete impact control row is invalid", source)
                })?;
                match kind.as_str() {
                    "run" => {
                        runs.insert(
                            assistant_protocol::RunId::new(id)
                                .map_err(|_| invalid_data("delete run ID is invalid"))?,
                        );
                    }
                    "child" => {
                        children.insert(
                            assistant_protocol::ChildTaskId::new(id)
                                .map_err(|_| invalid_data("delete child ID is invalid"))?,
                        );
                    }
                    "attachment" => {
                        attachments.insert(
                            assistant_protocol::AttachmentId::new(id)
                                .map_err(|_| invalid_data("delete attachment ID is invalid"))?,
                        );
                    }
                    _ => return Err(invalid_data("delete impact control kind is invalid")),
                }
            }
        }
        if !self.allow_legacy_history {
            for child in self.pending_child_catalog(session_id)? {
                children.insert(child.child_task_id);
            }
        }
        let path = body_path(&self.session_directory(session_id)?, generation);
        self.conversation_indexes.deletion_impact(
            &path,
            &owner,
            generation,
            &runs,
            &children,
            &attachments,
        )
    }

    pub(super) fn delete_session(&mut self, deletion: SessionDeletion) -> StorageResult<()> {
        super::filesystem::validate_session_component(&deletion.session_id)?;
        let role = self
            .connection
            .query_row(
                "SELECT role FROM sessions WHERE session_id = ?1",
                [deletion.session_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| internal_error("delete session role could not be queried", source))?
            .ok_or_else(|| conflict("delete session does not exist"))?;
        if role != "standard" {
            return Err(conflict("session role cannot be deleted"));
        }
        self.ensure_session_idle_for_transfer(&deletion.session_id)?;
        if self.inspect_session_deletion(&deletion.session_id)? != deletion.expected_impact {
            return Err(conflict("delete session impact changed"));
        }
        let source = self.session_directory(&deletion.session_id)?;
        let staged = self
            .deletion_staging_directory
            .join(format!("{}.{}", deletion.session_id, deletion.operation_id));
        if staged.exists() {
            return Err(conflict("delete staging path already exists"));
        }
        self.conversation_indexes.remove_under(&source);
        fs::rename(&source, &staged).map_err(|source| {
            internal_error("session directory could not enter delete staging", source)
        })?;
        sync_directory(&self.sessions_directory)?;
        sync_directory(&self.deletion_staging_directory)?;

        let deleted = (|| -> StorageResult<()> {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| {
                    database_write_error("delete transaction could not begin", source)
                })?;
            let changed = transaction
                .execute(
                    "DELETE FROM sessions WHERE session_id = ?1",
                    [deletion.session_id.as_str()],
                )
                .map_err(|source| database_write_error("session could not be deleted", source))?;
            if changed != 1 {
                return Err(conflict("delete session does not exist"));
            }
            transaction.commit().map_err(|source| {
                database_write_error("delete transaction could not commit", source)
            })
        })();
        if let Err(error) = deleted {
            if fs::rename(&staged, &source).is_err() {
                return Err(invalid_data(
                    "failed delete could not restore session directory",
                ));
            }
            let _ = sync_directory(&self.sessions_directory);
            let _ = sync_directory(&self.deletion_staging_directory);
            return Err(error);
        }
        self.unavailable_sessions
            .remove(deletion.session_id.as_str());
        if fs::remove_dir_all(&staged).is_ok() {
            let _ = sync_directory(&self.deletion_staging_directory);
        }
        Ok(())
    }

    pub(super) fn recover_session_deletions(&mut self) -> StorageResult<()> {
        for entry in fs::read_dir(&self.deletion_staging_directory).map_err(|source| {
            internal_error("delete staging directory could not be read", source)
        })? {
            let entry = entry.map_err(|source| {
                internal_error("delete staging entry could not be read", source)
            })?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| invalid_data("delete staging entry name is invalid"))?;
            let (session, _) = name
                .split_once('.')
                .ok_or_else(|| invalid_data("delete staging entry name is invalid"))?;
            let session_id = SessionId::new(session.to_owned())
                .map_err(|_| invalid_data("delete staging session id is invalid"))?;
            super::filesystem::validate_session_component(&session_id)?;
            let exists = self
                .connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
                    [session_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|source| {
                    internal_error("delete recovery session could not be queried", source)
                })?
                == 1;
            let staged = entry.path();
            if exists {
                let target = self.sessions_directory.join(session_id.as_str());
                if target.exists() {
                    return Err(invalid_data(
                        "delete recovery found duplicate session directories",
                    ));
                }
                fs::rename(&staged, &target).map_err(|source| {
                    internal_error(
                        "delete recovery could not restore session directory",
                        source,
                    )
                })?;
            } else {
                fs::remove_dir_all(&staged).map_err(|source| {
                    internal_error("committed delete staging could not be removed", source)
                })?;
            }
        }
        sync_directory(&self.sessions_directory)?;
        sync_directory(&self.deletion_staging_directory)
    }

    fn ensure_session_idle_for_transfer(&self, session_id: &SessionId) -> StorageResult<()> {
        let busy = self
            .connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM inputs WHERE session_id = ?1 AND state = 'queued'
                    UNION ALL
                    SELECT 1 FROM runs WHERE session_id = ?1
                        AND status IN ('accepted', 'running', 'cancelling')
                    UNION ALL
                    SELECT 1 FROM child_tasks WHERE session_id = ?1
                        AND status IN ('accepted', 'running')
                    UNION ALL
                    SELECT 1 FROM pending_tool_exchanges WHERE session_id = ?1
                    UNION ALL
                    SELECT 1 FROM child_pending_tool_exchanges WHERE session_id = ?1
                 )",
                [session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| {
                internal_error("session transfer state could not be queried", source)
            })?;
        if busy == 1 {
            return Err(conflict("session is not idle"));
        }
        Ok(())
    }
}

fn rewrite_file_reference_paths(
    conversation: &mut ConversationSnapshot,
    rewrites: &BTreeMap<String, String>,
) -> StorageResult<()> {
    for message in &mut conversation.messages {
        let ConversationMessage::User(user) = message else {
            continue;
        };
        for part in &mut user.parts {
            let UserPart::FileReferences(references) = part else {
                continue;
            };
            for file in &mut references.files {
                file.readable_path =
                    rewrites.get(&file.readable_path).cloned().ok_or_else(|| {
                        conflict("fork conversation references an unmapped attachment")
                    })?;
            }
        }
    }
    Ok(())
}

/// 目标正文已经完成结构化路径改绑；只冻结本 owner 实际引用的附件，不保留来源路径。
fn fork_attachment_history(
    conversation: &ConversationSnapshot,
    attachments: &[StoredAttachment],
    directory: &Path,
) -> StorageResult<Vec<assistant_runtime::StoredHistoryAttachment>> {
    let paths: BTreeSet<_> = conversation
        .messages
        .iter()
        .filter_map(|message| {
            if let ConversationMessage::User(user) = message {
                Some(&user.parts)
            } else {
                None
            }
        })
        .flatten()
        .filter_map(|part| {
            if let UserPart::FileReferences(references) = part {
                Some(&references.files)
            } else {
                None
            }
        })
        .flatten()
        .map(|file| file.readable_path.as_str())
        .collect();
    attachments
        .iter()
        .filter(|attachment| paths.contains(attachment.agent_readable_path.as_str()))
        .map(|attachment| {
            let relative = Path::new(&attachment.agent_readable_path)
                .strip_prefix(directory)
                .map_err(|_| invalid_data("fork attachment escapes its managed owner"))?;
            Ok(assistant_runtime::StoredHistoryAttachment {
                attachment_id: attachment.attachment_id.clone(),
                blob_hash: attachment.blob_hash.clone(),
                original_name: attachment.original_name.clone(),
                size_bytes: attachment.size_bytes,
                media_type: attachment.media_type.clone(),
                relative_path: relative
                    .to_str()
                    .ok_or_else(|| invalid_data("fork attachment path is not UTF-8"))?
                    .replace('\\', "/"),
            })
        })
        .collect()
}

fn path_text(path: &Path) -> StorageResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| StoreError::new(StoreErrorKind::InvalidInput, "path must be UTF-8"))
}
