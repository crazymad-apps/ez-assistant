use super::*;

impl VolatileRuntimeStore {
    pub(super) fn set_session_archive(&self, change: ArchiveChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if change.archived {
                ensure_idle(&state, &change.session_id)?;
            }
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
            if session.role != SessionRole::Standard {
                return Err(conflict("session role cannot be archived"));
            }
            match (session.lifecycle, change.archived) {
                (StoredSessionLifecycle::Active, true) => {
                    session.lifecycle = StoredSessionLifecycle::Archived;
                    session.archived_at_ms = Some(change.changed_at_ms);
                }
                (StoredSessionLifecycle::Archived, false) => {
                    session.lifecycle = StoredSessionLifecycle::Active;
                    session.archived_at_ms = None;
                }
                _ => return Err(conflict("session lifecycle cannot be changed")),
            }
            Ok(())
        })
    }

    pub(super) fn set_session_proxy(&self, change: SessionProxyChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let controller = state
                .sessions
                .get(&change.controller_session_id)
                .ok_or_else(|| conflict("controller session does not exist"))?;
            if controller.role != SessionRole::Controller
                || controller.lifecycle != StoredSessionLifecycle::Active
                || change.target_session_id == change.controller_session_id
            {
                return Err(conflict("controller session is invalid"));
            }
            let target = state
                .sessions
                .get_mut(&change.target_session_id)
                .ok_or_else(|| conflict("proxy target session does not exist"))?;
            if target.role != SessionRole::Standard
                || target.lifecycle != StoredSessionLifecycle::Active
            {
                return Err(conflict("proxy target session is invalid"));
            }
            match (&target.proxy, change.enabled) {
                (Some(current), true)
                    if current.controller_session_id == change.controller_session_id => {}
                (None, true) => {
                    target.proxy = Some(SessionProxyState {
                        controller_session_id: change.controller_session_id,
                        changed_at_ms: change.changed_at_ms,
                    });
                }
                (Some(current), false)
                    if current.controller_session_id == change.controller_session_id =>
                {
                    target.proxy = None;
                }
                (None, false) => {}
                _ => return Err(conflict("proxy target is bound to another controller")),
            }
            Ok(())
        })
    }

    pub(super) fn rename_session(&self, change: SessionTitleChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            session.title = change.title;
            session.title_origin = SessionTitleOrigin::User;
            session.automatic_title_pending = false;
            Ok(())
        })
    }

    pub(super) fn disable_automatic_title(&self, session_id: &SessionId) -> StoreFuture<'_, ()> {
        let session_id = session_id.clone();
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get_mut(&session_id)
                .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            session.automatic_title_pending = false;
            Ok(())
        })
    }

    pub(super) fn commit_session_title_generation(
        &self,
        commit: SessionTitleGenerationCommit,
    ) -> StoreFuture<'_, SessionTitleGenerationCommitResult> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get_mut(&commit.session_id)
                .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            let may_apply = match commit.trigger {
                SessionTitleGenerationTriggerSnapshot::Automatic => {
                    session.title_origin == SessionTitleOrigin::Generated
                        && commit.expected_title.as_deref() == Some(session.title.as_str())
                }
                SessionTitleGenerationTriggerSnapshot::Manual => true,
            };
            let applied = may_apply && commit.title.is_some();
            if let Some(title) = commit.title.filter(|_| applied) {
                session.title = title;
                session.title_origin = SessionTitleOrigin::Generated;
            }
            session.automatic_title_pending = false;
            let title = session.title.clone();
            let title_origin = session.title_origin;
            let automatic_title_pending = session.automatic_title_pending;

            if commit.request_attempted {
                let usage = state.session_usage.entry(commit.session_id).or_default();
                usage.auxiliary_request_count = usage.auxiliary_request_count.saturating_add(1);
                if let Some(tokens) = commit.usage {
                    usage.auxiliary_input_tokens = usage
                        .auxiliary_input_tokens
                        .saturating_add(tokens.input_tokens);
                    usage.auxiliary_output_tokens = usage
                        .auxiliary_output_tokens
                        .saturating_add(tokens.output_tokens);
                    usage.auxiliary_total_tokens = usage
                        .auxiliary_total_tokens
                        .saturating_add(tokens.total_tokens);
                }
            }
            Ok(SessionTitleGenerationCommitResult {
                applied,
                title,
                title_origin,
                automatic_title_pending,
            })
        })
    }

    pub(super) fn set_session_pinned(&self, change: SessionPinnedChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            session.is_pinned = change.is_pinned;
            Ok(())
        })
    }

    pub(super) fn set_session_model(&self, change: ModelChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            ensure_idle(&state, &change.session_id)?;
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            session.model_selection = change.model_selection;
            session.reasoning_effort = change.reasoning_effort;
            Ok(())
        })
    }

    pub(super) fn set_session_reasoning_effort(
        &self,
        change: ReasoningEffortChange,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            session.reasoning_effort = change.reasoning_effort;
            Ok(())
        })
    }

    pub(super) fn set_session_variant(&self, change: VariantChange) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            session.current_variant = change.variant;
            Ok(())
        })
    }

    pub(super) fn set_session_approval_mode(
        &self,
        change: ApprovalModeChange,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get_mut(&change.session_id)
                .ok_or_else(|| conflict("session does not exist"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("session is archived"));
            }
            session.approval_mode = change.approval_mode;
            Ok(())
        })
    }
}
