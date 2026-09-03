//! 新会话首次发送的领域准备、可靠物化与进程内投影。

use std::{collections::BTreeSet, path::Path, sync::Arc};

use agent_types::FileReference;
use assistant_protocol::{
    SessionMaterializationManifest, SessionMaterializationResult, SessionTitleOrigin,
    SubmitInputMode,
};

use super::{
    AssistantRuntime, StagedSessionAttachment,
    attachment::summary as attachment_summary,
    goal::GoalSubmissionPersistence,
    input::{automatic_session_title, projection::project_accepted_input},
    model::resolve_session_model_key,
    quote::{deactivate_quote_sources, insert_quotes, validate_quotes},
    session_management::supports_effort,
};
use crate::{
    InputChannelSource, InputOrigin, NewAttachmentUpload, NewStoredInput, NewStoredSession,
    NewStoredSessionMaterialization, RuntimeError, RuntimeResult, SessionEnvironmentFactoryRequest,
    SkillActivationOwner, SkillActivationResolveError, SkillActivationTrigger, SkillName,
    StoredMcpSelection, StoredSkillActivation, WorkspaceEnvironmentSource,
    attachment_stable_view_path, id,
    internal_boundary::{
        InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
    },
    run::{allocate_run_id, create_user_message},
    session::{SessionController, allocate_session_id},
    skill::render_user_activation,
};

const MAX_MATERIALIZATION_ATTACHMENTS: usize = 32;

impl AssistantRuntime {
    /// 将草稿首次发送一次性落实为正式 Session、附件、Input 和首次 Run。
    pub async fn materialize_session(
        &self,
        manifest: SessionMaterializationManifest,
        staged_attachments: Vec<StagedSessionAttachment>,
    ) -> RuntimeResult<SessionMaterializationResult> {
        let _operation = self.operation_gate.read().await;
        let _binding = self.model_binding_gate.read().await;
        self.ensure_running()?;
        let _workspace_mutation = self.workspace_mutation_gate.lock().await;
        validate_manifest(&manifest, &staged_attachments)?;

        let configuration = self.config_registry.snapshot()?;
        let model_key = resolve_session_model_key(&configuration, manifest.model_key.clone())?;
        let model = configuration
            .active()
            .and_then(|active| active.model(&model_key))
            .ok_or_else(|| RuntimeError::ModelUnavailable {
                model_key: model_key.clone(),
            })?;
        if manifest
            .reasoning_effort
            .is_some_and(|effort| !supports_effort(model.capabilities(), effort))
        {
            return Err(RuntimeError::InvalidRequest {
                reason: "reasoning effort is not supported by the current model",
            });
        }

        let session_id = {
            let sessions =
                self.sessions
                    .read()
                    .map_err(|_| RuntimeError::InternalStateUnavailable {
                        component: "session registry",
                    })?;
            allocate_session_id(&sessions)?
        };
        let workspace = manifest
            .workspace_id
            .as_ref()
            .map(|workspace_id| self.workspace_for_new_session(workspace_id))
            .transpose()?;
        let mut permission_scopes = vec![crate::PermissionFileScope::Global];
        if let Some(workspace_id) = workspace
            .as_ref()
            .map(|workspace| workspace.workspace_id.clone())
        {
            permission_scopes.push(crate::PermissionFileScope::Workspace(workspace_id));
        }
        let selected_mcp = manifest
            .mcp_server_key
            .as_ref()
            .map(|server_key| {
                let server = self
                    .mcp_service
                    .registry
                    .catalog_server(server_key)?
                    .ok_or(RuntimeError::McpServerUnavailable)?;
                let visible = server.tools.iter().any(|tool| {
                    self.permission_coordinator
                        .mcp_tool_is_explicitly_denied(
                            &permission_scopes,
                            manifest.variant,
                            server_key,
                            &tool.name,
                        )
                        .is_ok_and(|denied| !denied)
                });
                if !visible {
                    return Err(RuntimeError::McpServerUnavailable);
                }
                Ok((server_key.clone(), server.display_name))
            })
            .transpose()?;
        let memory_context = self
            .store
            .load_memory_context()
            .await
            .map_err(|source| RuntimeError::from_store("load memory context", source))?;
        let prepared_environment = self
            .session_environment_factory
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &session_id,
                workspace: workspace
                    .as_ref()
                    .map(|workspace| WorkspaceEnvironmentSource {
                        workspace_id: &workspace.workspace_id,
                        label: &workspace.label,
                        user_directory: &workspace.user_directory,
                        additional_directories: &workspace.additional_directories,
                        agent_directory: &workspace.agent_directory,
                    }),
                memory_context: &memory_context,
            })
            .map_err(|source| RuntimeError::SessionEnvironmentBuildFailed { source })?;
        let skill_catalog = self
            .prepare_session_skill_catalog(workspace.as_ref().map(super::workspace_directories))
            .await?;
        let system_prompt = skill_catalog.augment_system_prompt(prepared_environment.system_prompt);
        let created_at_ms = super::now_ms()?;
        let new_session = NewStoredSession {
            session_id: session_id.clone(),
            title: automatic_session_title(&manifest.message),
            title_origin: SessionTitleOrigin::Generated,
            model_key,
            reasoning_effort: manifest.reasoning_effort,
            system_prompt,
            skill_catalog,
            environment: prepared_environment.environment,
            current_variant: manifest.variant,
            approval_mode: manifest.approval_mode,
            role: crate::SessionRole::Standard,
            materialization_key: Some(manifest.idempotency_key.clone()),
            automatic_title_pending: super::automatic_title_enabled_by_default(),
            created_at_ms,
        };
        let provisional = Arc::new(SessionController::new(stored_session_preview(&new_session)));
        let goal_submission = self.goal_submission(&provisional, manifest.mode)?;

        let (input_id, run_id) = {
            let state = provisional.lock_state()?;
            (self.allocate_input_id(&state)?, allocate_run_id(&state)?)
        };
        let mut uploads = Vec::with_capacity(staged_attachments.len());
        let mut file_references = Vec::with_capacity(staged_attachments.len());
        let mut allocated_attachment_ids = BTreeSet::new();
        for staged in staged_attachments {
            let attachment_id = self.allocate_attachment_id()?;
            if !allocated_attachment_ids.insert(attachment_id.clone()) {
                return Err(RuntimeError::InternalStateUnavailable {
                    component: "materialization attachment id collision",
                });
            }
            let readable_path = attachment_stable_view_path(
                Path::new(&new_session.environment.session_attachment_directory),
                &attachment_id,
                &staged.original_name,
            )
            .to_str()
            .ok_or(RuntimeError::InvalidRequest {
                reason: "attachment path must be UTF-8",
            })?
            .to_owned();
            file_references.push(FileReference {
                original_name: staged.original_name.clone(),
                readable_path,
            });
            uploads.push(NewAttachmentUpload {
                attachment_id,
                session_id: session_id.clone(),
                original_name: staged.original_name,
                staging_path: staged.staging_path,
                blob_hash: staged.blob_hash,
                size_bytes: staged.size_bytes,
                media_type: staged.media_type,
                created_at_ms,
            });
        }
        let quotes = deactivate_quote_sources(&manifest.quotes)?;
        let mut message = create_user_message(manifest.message, file_references, manifest.variant)?;
        insert_quotes(&mut message, &quotes)?;
        let mut prepared_goal = goal_submission.prepare(&mut message, created_at_ms)?;
        if let Some(goal) = prepared_goal.as_mut()
            && let Some((server_key, _)) = selected_mcp.as_ref()
        {
            goal.control.mcp_server_key = Some(server_key.clone());
        }
        let mcp_selection = selected_mcp
            .map(|(server_key, display_name)| {
                Ok(StoredMcpSelection {
                    selection_id: id::generate("mcp-selection").map_err(|_| {
                        RuntimeError::InternalStateUnavailable {
                            component: "MCP selection id random source",
                        }
                    })?,
                    session_id: session_id.clone(),
                    input_id: Some(input_id.clone()),
                    message_id: message.id.clone(),
                    server_key,
                    display_name,
                    created_at_ms,
                })
            })
            .transpose()?;
        let selected_skill = manifest
            .skill_name
            .map(|name| SkillName::parse(name).map_err(|_| RuntimeError::SkillNameInvalid))
            .transpose()?;
        let skill_activation = selected_skill
            .as_ref()
            .map(|name| {
                let definition = provisional.skill_catalog().user_definition(name).map_err(
                    |error| match error {
                        SkillActivationResolveError::CatalogUnavailable => {
                            RuntimeError::SkillCatalogUnavailable {
                                session_id: session_id.clone(),
                            }
                        }
                        SkillActivationResolveError::NotFound => RuntimeError::SkillNotFound {
                            session_id: session_id.clone(),
                        },
                        SkillActivationResolveError::NotUserInvocable => {
                            RuntimeError::SkillNotUserInvocable {
                                session_id: session_id.clone(),
                            }
                        }
                    },
                )?;
                InternalBoundaryCoordinator::append(
                    &mut message,
                    InternalBoundaryRequest {
                        source: InternalBoundarySource::SkillActivation,
                        text: render_user_activation(
                            &provisional.skill_catalog().revision,
                            definition,
                        ),
                    },
                )?;
                Ok(StoredSkillActivation {
                    activation_id: id::generate("skill-activation").map_err(|_| {
                        RuntimeError::InternalStateUnavailable {
                            component: "skill activation id random source",
                        }
                    })?,
                    session_id: session_id.clone(),
                    owner: SkillActivationOwner::Session(session_id.clone()),
                    run_id: Some(run_id.clone()),
                    input_id: Some(input_id.clone()),
                    message_id: message.id.clone(),
                    name: definition.name.clone(),
                    catalog_revision: provisional.skill_catalog().revision.clone(),
                    definition_digest: definition.definition_digest.clone(),
                    trigger: SkillActivationTrigger::User,
                    created_at_ms,
                })
            })
            .transpose()?;
        let goal_binding = prepared_goal.as_ref().map(|goal| goal.binding.clone());
        let new_goal = prepared_goal
            .as_ref()
            .filter(|goal| matches!(goal.persistence, GoalSubmissionPersistence::Start))
            .map(|goal| goal.control.to_stored(session_id.clone()));
        let input = NewStoredInput {
            input_id,
            run_id,
            session_id: session_id.clone(),
            idempotency_key: None,
            agent_variant: manifest.variant,
            origin: InputOrigin::User,
            goal_binding,
            cross_session: None,
            channel_source: Some(InputChannelSource::desktop_text()),
            skill_activation,
            mcp_selection: mcp_selection.clone(),
            approval_mode: manifest.approval_mode,
            message,
            new_goal,
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: created_at_ms,
        };
        let stored = self
            .store
            .materialize_session(NewStoredSessionMaterialization {
                session: new_session,
                attachments: uploads,
                input,
            })
            .await
            .map_err(|source| {
                if source.kind() == crate::StoreErrorKind::Conflict {
                    RuntimeError::MaterializationConflict
                } else {
                    RuntimeError::from_store("materialize session", source)
                }
            })?;

        if let Ok(existing) = self.session(&stored.session.session_id) {
            let state = existing.lock_state()?;
            let run = state
                .runs
                .get(&stored.accepted.run.run_id)
                .map(crate::run::RunRecord::snapshot)
                .unwrap_or_else(|| {
                    crate::run::RunRecord::accepted(&stored.accepted.run, Vec::new()).snapshot()
                });
            drop(state);
            return Ok(SessionMaterializationResult {
                session: existing.summary()?,
                input_id: stored.accepted.input.input_id,
                run,
                attachments: stored.attachments.iter().map(attachment_summary).collect(),
            });
        }

        self.permission_coordinator
            .register_scope(crate::PermissionFileScope::Session(
                stored.session.session_id.clone(),
            ))
            .await?;
        let controller = Arc::new(SessionController::new(stored.session));
        let goal_snapshot = stored
            .goal
            .map(crate::goal::GoalControl::try_from)
            .transpose()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "materialized Goal projection",
            })?;
        let mut accepted = stored.accepted;
        accepted.is_duplicate = false;
        let projection = {
            let mut state = controller.lock_state()?;
            state.goal = goal_snapshot.clone();
            project_accepted_input(&mut state, accepted, mcp_selection)
        };
        {
            let mut attachments =
                self.attachments
                    .write()
                    .map_err(|_| RuntimeError::InternalStateUnavailable {
                        component: "attachment registry",
                    })?;
            for attachment in &stored.attachments {
                attachments.insert(attachment.attachment_id.clone(), attachment.clone());
            }
        }
        self.sessions
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session registry",
            })?
            .insert(session_id.clone(), controller.clone());
        let session = controller.summary()?;
        self.publish(assistant_protocol::RuntimeEvent::SessionCreated {
            session: session.clone(),
        });
        self.publish(assistant_protocol::RuntimeEvent::RunAccepted {
            session_id: session_id.clone(),
            run_id: projection.run.run_id.clone(),
        });
        if let Some(goal) = goal_snapshot.as_ref() {
            let goal = super::product::project_goal(goal)?;
            self.publish(assistant_protocol::RuntimeEvent::GoalChanged {
                session_id: session_id.clone(),
                goal_id: goal.goal_id,
                generation: goal.generation,
            });
        }
        if let Some(revision) = projection.queue_revision {
            self.publish(assistant_protocol::RuntimeEvent::QueueChanged {
                session_id: session_id.clone(),
                revision,
            });
        }
        self.wake_queue(controller)?;
        Ok(SessionMaterializationResult {
            session,
            input_id: projection.input_id,
            run: projection.run,
            attachments: stored.attachments.iter().map(attachment_summary).collect(),
        })
    }
}

fn validate_manifest(
    manifest: &SessionMaterializationManifest,
    staged: &[StagedSessionAttachment],
) -> RuntimeResult<()> {
    if manifest.mode == SubmitInputMode::ResumeGoal {
        return Err(RuntimeError::InvalidRequest {
            reason: "new session cannot resume a Goal",
        });
    }
    if manifest.attachments.len() > MAX_MATERIALIZATION_ATTACHMENTS
        || manifest.attachments.len() != staged.len()
    {
        return Err(RuntimeError::InvalidRequest {
            reason: "materialization attachment count is invalid",
        });
    }
    if manifest.message.trim().is_empty()
        && manifest.attachments.is_empty()
        && manifest.quotes.is_empty()
    {
        return Err(RuntimeError::InvalidRequest {
            reason: "materialization input must not be empty",
        });
    }
    let mut selection_keys = BTreeSet::new();
    let mut blob_hashes = BTreeSet::new();
    for (declared, actual) in manifest.attachments.iter().zip(staged) {
        if declared.selection_key.trim().is_empty()
            || !selection_keys.insert(declared.selection_key.as_str())
            || declared.selection_key != actual.selection_key
            || declared.original_name != actual.original_name
            || declared.size_bytes != actual.size_bytes
            || !blob_hashes.insert(actual.blob_hash.as_str())
        {
            return Err(RuntimeError::InvalidRequest {
                reason: "materialization attachment manifest does not match upload",
            });
        }
    }
    validate_quotes(&manifest.quotes)
}

fn stored_session_preview(session: &NewStoredSession) -> crate::StoredSession {
    crate::StoredSession {
        session_id: session.session_id.clone(),
        title: session.title.clone(),
        title_origin: session.title_origin,
        model_key: session.model_key.clone(),
        reasoning_effort: session.reasoning_effort,
        system_prompt: session.system_prompt.clone(),
        skill_catalog: session.skill_catalog.clone(),
        environment: session.environment.clone(),
        lifecycle: crate::StoredSessionLifecycle::Active,
        current_variant: session.current_variant,
        approval_mode: session.approval_mode,
        role: session.role,
        materialization_key: session.materialization_key.clone(),
        automatic_title_pending: session.automatic_title_pending,
        proxy: None,
        pc_output_hosting: None,
        body_generation: 1,
        message_count: 0,
        created_at_ms: session.created_at_ms,
        updated_at_ms: session.created_at_ms,
        archived_at_ms: None,
        is_pinned: false,
        conversation_state: crate::StoredConversationState::Available,
    }
}
