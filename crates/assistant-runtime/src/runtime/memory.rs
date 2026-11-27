//! Persona、Pinned Memory 与冻结 System Context 的产品命令。

use agent_memory::{PinnedMemoryCategory, PinnedMemoryEntry, PinnedMemoryId};
use agent_model::SystemPromptSnapshot;
use assistant_protocol::{
    ControllerPromptSnapshot as ControllerPromptProjection, CreatePinnedMemoryRequest,
    DeletePinnedMemoryRequest, GetControllerPromptRequest, GetControllerPromptResult,
    GetMemoryCapabilitiesRequest, GetMemoryCapabilitiesResult, GetPersonaRequest, GetPersonaResult,
    GetSystemContextRequest, GetSystemContextResult, ListPinnedMemoriesRequest,
    ListPinnedMemoriesResult, MemoryCapabilities, PersonaSnapshot as PersonaProjection,
    PinnedMemoryCollectionSnapshot, PinnedMemoryCreatedBy as CreatedByProjection,
    PinnedMemoryMutationResult as MutationProjection, PinnedMemorySnapshot,
    SetControllerPromptRequest, SetControllerPromptResult, SetPersonaRequest, SetPersonaResult,
    SystemContextSnapshot, UpdatePinnedMemoryRequest,
};

use super::{AssistantRuntime, now_ms};
use crate::{
    ControllerPromptMutation, ControllerPromptSnapshot, MAX_CONTROLLER_PROMPT_BYTES,
    PersonaMutation, PinnedMemoryCreatedBy, PinnedMemoryMutation, RuntimeError, RuntimeResult,
    SessionRole, StoreErrorKind, StoredPinnedMemory,
    memory::{MAX_PERSONA_BYTES, domain_attributes, protocol_attributes},
    pinned_memory_limits,
};

impl AssistantRuntime {
    pub async fn get_memory_capabilities(
        &self,
        _request: GetMemoryCapabilitiesRequest,
    ) -> RuntimeResult<GetMemoryCapabilitiesResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        Ok(GetMemoryCapabilitiesResult {
            capabilities: memory_capabilities(),
        })
    }

    pub async fn get_persona(
        &self,
        _request: GetPersonaRequest,
    ) -> RuntimeResult<GetPersonaResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let persona = self
            .store
            .get_persona()
            .await
            .map_err(|source| RuntimeError::from_store("load persona", source))?;
        Ok(GetPersonaResult {
            persona: project_persona(persona),
            capabilities: memory_capabilities(),
        })
    }

    pub async fn set_persona(&self, request: SetPersonaRequest) -> RuntimeResult<SetPersonaResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        validate_persona(request.enabled, &request.content)?;
        let mutation = PersonaMutation {
            expected_revision: request.expected_revision,
            enabled: request.enabled,
            content: request.content,
            updated_at_ms: now_ms()?,
        };
        match self.store.set_persona(mutation).await {
            Ok(persona) => Ok(SetPersonaResult {
                applied: true,
                persona: project_persona(persona),
            }),
            Err(source) if source.kind() == StoreErrorKind::Conflict => {
                let persona = self
                    .store
                    .get_persona()
                    .await
                    .map_err(|source| RuntimeError::from_store("reload persona", source))?;
                Ok(SetPersonaResult {
                    applied: false,
                    persona: project_persona(persona),
                })
            }
            Err(source) => Err(RuntimeError::from_store("update persona", source)),
        }
    }

    pub async fn get_controller_prompt(
        &self,
        _request: GetControllerPromptRequest,
    ) -> RuntimeResult<GetControllerPromptResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let prompt = self
            .store
            .get_controller_prompt()
            .await
            .map_err(|source| RuntimeError::from_store("load controller prompt", source))?;
        Ok(GetControllerPromptResult {
            prompt: project_controller_prompt(prompt),
            max_bytes: MAX_CONTROLLER_PROMPT_BYTES as u32,
        })
    }

    pub async fn set_controller_prompt(
        &self,
        request: SetControllerPromptRequest,
    ) -> RuntimeResult<SetControllerPromptResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        validate_controller_prompt(request.enabled, &request.content)?;
        let mutation = ControllerPromptMutation {
            expected_revision: request.expected_revision,
            enabled: request.enabled,
            content: request.content,
            updated_at_ms: now_ms()?,
        };
        match self.store.set_controller_prompt(mutation).await {
            Ok(prompt) => {
                let key = crate::id::generate("history").map_err(|_| {
                    RuntimeError::InternalStateUnavailable {
                        component: "history binding",
                    }
                })?;
                self.store
                    .invalidate_controller_server_history_bindings(key.clone())
                    .await
                    .map_err(|source| {
                        RuntimeError::from_store("invalidate controller history bindings", source)
                    })?;
                for session in self
                    .sessions
                    .read()
                    .map_err(|_| RuntimeError::InternalStateUnavailable {
                        component: "sessions",
                    })?
                    .values()
                {
                    if session.role()? == SessionRole::Controller {
                        session.lock_state()?.server_history_key = Some(key.clone());
                    }
                }
                Ok(SetControllerPromptResult {
                    applied: true,
                    prompt: project_controller_prompt(prompt),
                })
            }
            Err(source) if source.kind() == StoreErrorKind::Conflict => {
                let prompt = self.store.get_controller_prompt().await.map_err(|source| {
                    RuntimeError::from_store("reload controller prompt", source)
                })?;
                Ok(SetControllerPromptResult {
                    applied: false,
                    prompt: project_controller_prompt(prompt),
                })
            }
            Err(source) => Err(RuntimeError::from_store("update controller prompt", source)),
        }
    }

    pub async fn list_pinned_memories(
        &self,
        _request: ListPinnedMemoriesRequest,
    ) -> RuntimeResult<ListPinnedMemoriesResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        Ok(ListPinnedMemoriesResult {
            collection: self.load_pinned_collection().await?,
        })
    }

    pub async fn create_pinned_memory(
        &self,
        request: CreatePinnedMemoryRequest,
    ) -> RuntimeResult<MutationProjection> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let limits = pinned_memory_limits();
        let current = self.load_pinned_collection().await?;
        if current.items.len() >= limits.max_entries.get() {
            return Err(RuntimeError::InvalidRequest {
                reason: "pinned memory entry limit reached",
            });
        }
        let entry = PinnedMemoryEntry {
            id: allocate_memory_id()?,
            category: PinnedMemoryCategory::new(request.category).map_err(|_| invalid_memory())?,
            content: request.content,
            attributes: domain_attributes(request.attributes).map_err(|_| invalid_memory())?,
        };
        entry.validate(&limits).map_err(|_| invalid_memory())?;
        self.apply_pinned_mutation(
            PinnedMemoryMutation::Create {
                entry,
                created_by: PinnedMemoryCreatedBy::User,
                expected_collection_revision: request.expected_collection_revision,
                changed_at_ms: now_ms()?,
            },
            "create pinned memory",
        )
        .await
    }

    pub async fn update_pinned_memory(
        &self,
        request: UpdatePinnedMemoryRequest,
    ) -> RuntimeResult<MutationProjection> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let entry = PinnedMemoryEntry {
            id: PinnedMemoryId::new(request.id).map_err(|_| invalid_memory())?,
            category: PinnedMemoryCategory::new(request.category).map_err(|_| invalid_memory())?,
            content: request.content,
            attributes: domain_attributes(request.attributes).map_err(|_| invalid_memory())?,
        };
        entry
            .validate(&pinned_memory_limits())
            .map_err(|_| invalid_memory())?;
        self.apply_pinned_mutation(
            PinnedMemoryMutation::Replace {
                entry,
                expected_revision: request.expected_revision,
                changed_at_ms: now_ms()?,
            },
            "update pinned memory",
        )
        .await
    }

    pub async fn delete_pinned_memory(
        &self,
        request: DeletePinnedMemoryRequest,
    ) -> RuntimeResult<MutationProjection> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let id = PinnedMemoryId::new(request.id).map_err(|_| invalid_memory())?;
        self.apply_pinned_mutation(
            PinnedMemoryMutation::Delete {
                id,
                expected_revision: request.expected_revision,
                changed_at_ms: now_ms()?,
            },
            "delete pinned memory",
        )
        .await
    }

    pub async fn get_system_context(
        &self,
        request: GetSystemContextRequest,
    ) -> RuntimeResult<GetSystemContextResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id).await?;
        let summary = session.summary()?;
        let base = session.current_system_prompt()?;
        let prompt = if session.role()? == SessionRole::Controller {
            let controller_prompt = self
                .store
                .get_controller_prompt()
                .await
                .map_err(|source| RuntimeError::from_store("load controller prompt", source))?;
            merge_controller_prompt(&base, Some(&controller_prompt))
        } else {
            base
        };
        Ok(GetSystemContextResult {
            snapshot: SystemContextSnapshot {
                session_id: request.session_id,
                session_created_at_ms: summary.created_at_ms,
                workspace: self.session_workspace_snapshot(&session)?,
                parts: prompt.parts().to_vec(),
            },
        })
    }

    async fn apply_pinned_mutation(
        &self,
        mutation: PinnedMemoryMutation,
        operation: &'static str,
    ) -> RuntimeResult<MutationProjection> {
        match self.store.mutate_pinned_memory(mutation).await {
            Ok(result) => {
                let memory = result.memory.map(project_memory);
                Ok(MutationProjection {
                    applied: true,
                    memory,
                    collection: self.load_pinned_collection().await?,
                })
            }
            Err(source) if source.kind() == StoreErrorKind::Conflict => Ok(MutationProjection {
                applied: false,
                memory: None,
                collection: self.load_pinned_collection().await?,
            }),
            Err(source) => Err(RuntimeError::from_store(operation, source)),
        }
    }

    async fn load_pinned_collection(&self) -> RuntimeResult<PinnedMemoryCollectionSnapshot> {
        let context = self
            .store
            .load_memory_context()
            .await
            .map_err(|source| RuntimeError::from_store("load pinned memories", source))?;
        Ok(PinnedMemoryCollectionSnapshot {
            revision: context.pinned_collection_revision,
            items: context
                .pinned_memories
                .into_iter()
                .map(project_memory)
                .collect(),
            capabilities: memory_capabilities(),
        })
    }
}

pub(crate) fn merge_controller_prompt(
    base: &SystemPromptSnapshot,
    controller_prompt: Option<&ControllerPromptSnapshot>,
) -> SystemPromptSnapshot {
    let Some(controller_prompt) = controller_prompt else {
        return base.clone();
    };
    let trimmed = controller_prompt.content.trim();
    if !controller_prompt.enabled || trimmed.is_empty() {
        return base.clone();
    }
    let instruction = format!("<controller_instructions>\n{trimmed}\n</controller_instructions>");
    let mut parts = base.parts().to_vec();
    if let Some(directory_index) = parts
        .iter()
        .rposition(|part| part.starts_with("<runtime_directories>"))
    {
        parts.insert(directory_index, instruction);
    } else {
        parts.push(instruction);
    }
    SystemPromptSnapshot::new(parts)
}

fn validate_persona(_enabled: bool, content: &str) -> RuntimeResult<()> {
    validate_prompt_bytes(
        content,
        MAX_PERSONA_BYTES,
        "persona content is invalid or exceeds its capacity",
    )
}

fn validate_controller_prompt(_enabled: bool, content: &str) -> RuntimeResult<()> {
    validate_prompt_bytes(
        content,
        MAX_CONTROLLER_PROMPT_BYTES,
        "controller prompt content is invalid or exceeds its capacity",
    )
}

fn validate_prompt_bytes(
    content: &str,
    max_bytes: usize,
    reason: &'static str,
) -> RuntimeResult<()> {
    let contains_unsupported_control = content
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'));
    if content.len() > max_bytes || contains_unsupported_control {
        return Err(RuntimeError::InvalidRequest { reason });
    }
    Ok(())
}

fn project_controller_prompt(prompt: ControllerPromptSnapshot) -> ControllerPromptProjection {
    ControllerPromptProjection {
        enabled: prompt.enabled,
        content: prompt.content,
        revision: prompt.revision,
        updated_at_ms: prompt.updated_at_ms,
    }
}

fn invalid_memory() -> RuntimeError {
    RuntimeError::InvalidRequest {
        reason: "pinned memory input is invalid or exceeds its capacity",
    }
}

fn allocate_memory_id() -> RuntimeResult<PinnedMemoryId> {
    let value = crate::id::generate("pm").map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "pinned memory id random source",
    })?;
    PinnedMemoryId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "pinned memory id generator",
    })
}

fn memory_capabilities() -> MemoryCapabilities {
    let limits = pinned_memory_limits();
    MemoryCapabilities {
        max_persona_bytes: MAX_PERSONA_BYTES as u32,
        max_pinned_entries: limits.max_entries.get() as u32,
        max_pinned_category_bytes: limits.max_category_bytes.get() as u32,
        max_pinned_content_bytes: limits.max_content_bytes.get() as u32,
        max_attributes_per_entry: limits.max_attributes_per_entry.get() as u32,
        max_attribute_key_bytes: limits.max_attribute_key_bytes.get() as u32,
        max_attribute_string_bytes: limits.max_attribute_string_bytes.get() as u32,
    }
}

fn project_persona(persona: crate::PersonaSnapshot) -> PersonaProjection {
    PersonaProjection {
        enabled: persona.enabled,
        content: persona.content,
        revision: persona.revision,
        updated_at_ms: persona.updated_at_ms,
    }
}

fn project_memory(memory: StoredPinnedMemory) -> PinnedMemorySnapshot {
    let created_by = match memory.created_by {
        PinnedMemoryCreatedBy::User => CreatedByProjection::User,
        PinnedMemoryCreatedBy::AgentTool { session_id } => {
            CreatedByProjection::AgentTool { session_id }
        }
    };
    PinnedMemorySnapshot {
        id: memory.entry.id.into_inner(),
        category: memory.entry.category.into_inner(),
        content: memory.entry.content,
        attributes: protocol_attributes(&memory.entry.attributes),
        created_by,
        created_at_ms: memory.created_at_ms,
        updated_at_ms: memory.updated_at_ms,
        revision: memory.revision,
    }
}
