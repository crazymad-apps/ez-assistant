//! Volatile Store 与 Host 使用相同可见性与范围语义；顺序仅属于存储私有状态。
use super::*;
use assistant_protocol::{
    MoveSidebarItemRequest, ResetSidebarOrderRequest, SessionGroup, SidebarPlacement,
};

#[derive(Default)]
pub(super) struct SidebarOrders {
    pub(super) workspaces: Vec<WorkspaceId>,
    pub(super) sessions: BTreeMap<Option<WorkspaceId>, Vec<SessionId>>,
    pub(super) pins: Vec<SessionId>,
}

/// 新创建条目排在手动顺序前，已有条目（含隐藏项）保持相对顺序。
pub(super) fn apply_order<T: Ord + Clone>(ids: &mut [T], order: &[T]) {
    let positions: BTreeMap<_, _> = order.iter().enumerate().map(|(i, id)| (id, i)).collect();
    ids.sort_by_key(|id| positions.get(id).map(|i| i + 1).unwrap_or(0));
}

fn workspace_ids(state: &State, manual: bool) -> Vec<WorkspaceId> {
    let mut rows: Vec<_> = state.workspaces.values().collect();
    rows.sort_by(|a, b| {
        b.created_at_ms
            .cmp(&a.created_at_ms)
            .then_with(|| a.workspace_id.cmp(&b.workspace_id))
    });
    let mut ids: Vec<_> = rows.iter().map(|w| w.workspace_id.clone()).collect();
    if manual {
        apply_order(&mut ids, &state.sidebar.workspaces);
    }
    ids
}

fn workspace(group: &SessionGroup) -> Option<WorkspaceId> {
    match group {
        SessionGroup::Workspace { workspace_id } => Some(workspace_id.clone()),
        SessionGroup::Unbound => None,
    }
}

fn session_ids(state: &State, group: &Option<WorkspaceId>, manual: bool) -> Vec<SessionId> {
    let mut rows: Vec<_> = state
        .sessions
        .values()
        .filter(|s| &s.environment.workspace_id == group)
        .collect();
    rows.sort_by(|a, b| {
        b.created_at_ms
            .cmp(&a.created_at_ms)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    let mut ids: Vec<_> = rows.iter().map(|s| s.session_id.clone()).collect();
    if manual {
        apply_order(
            &mut ids,
            state
                .sidebar
                .sessions
                .get(group)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
    }
    ids
}

fn ensure_workspace(state: &State, id: &Option<WorkspaceId>) -> Result<(), StoreError> {
    if id.as_ref().is_some_and(|id| {
        state
            .workspaces
            .get(id)
            .is_none_or(|w| w.lifecycle != StoredWorkspaceLifecycle::Active)
    }) {
        return Err(conflict("sidebar workspace is no longer available"));
    }
    Ok(())
}

fn ensure_session(
    state: &State,
    id: &SessionId,
    group: Option<&Option<WorkspaceId>>,
    pinned: bool,
) -> Result<(), StoreError> {
    let s = state
        .sessions
        .get(id)
        .ok_or_else(|| conflict("sidebar session is no longer available"))?;
    if s.lifecycle != StoredSessionLifecycle::Active
        || s.role != SessionRole::Standard
        || (pinned && !s.is_pinned)
        || group.is_some_and(|g| g != &s.environment.workspace_id)
    {
        return Err(conflict(
            "sidebar session is no longer available in this scope",
        ));
    }
    ensure_workspace(state, &s.environment.workspace_id)
}

fn move_item<T: PartialEq>(
    ids: &mut Vec<T>,
    id: &T,
    reference: &T,
    placement: SidebarPlacement,
) -> Result<(), StoreError> {
    if id == reference {
        return Err(conflict("sidebar move requires different items"));
    }
    let index = ids
        .iter()
        .position(|i| i == id)
        .ok_or_else(|| conflict("sidebar item changed"))?;
    let item = ids.remove(index);
    let reference = ids
        .iter()
        .position(|i| i == reference)
        .ok_or_else(|| conflict("sidebar reference changed"))?;
    ids.insert(
        reference + usize::from(placement == SidebarPlacement::After),
        item,
    );
    Ok(())
}

impl VolatileRuntimeStore {
    pub(super) fn move_sidebar_item(&self, request: MoveSidebarItemRequest) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            match request {
                MoveSidebarItemRequest::Workspaces {
                    workspace_id,
                    reference_id,
                    placement,
                } => {
                    ensure_workspace(&state, &Some(workspace_id.clone()))?;
                    ensure_workspace(&state, &Some(reference_id.clone()))?;
                    let mut ids = workspace_ids(&state, true);
                    move_item(&mut ids, &workspace_id, &reference_id, placement)?;
                    state.sidebar.workspaces = ids;
                }
                MoveSidebarItemRequest::Sessions {
                    group,
                    session_id,
                    reference_id,
                    placement,
                } => {
                    let group = workspace(&group);
                    ensure_session(&state, &session_id, Some(&group), false)?;
                    ensure_session(&state, &reference_id, Some(&group), false)?;
                    let mut ids = session_ids(&state, &group, true);
                    move_item(&mut ids, &session_id, &reference_id, placement)?;
                    state.sidebar.sessions.insert(group, ids);
                }
                MoveSidebarItemRequest::Pins {
                    session_id,
                    reference_id,
                    placement,
                } => {
                    ensure_session(&state, &session_id, None, true)?;
                    ensure_session(&state, &reference_id, None, true)?;
                    let mut ids = state.sidebar.pins.clone();
                    move_item(&mut ids, &session_id, &reference_id, placement)?;
                    state.sidebar.pins = ids;
                }
            }
            Ok(())
        })
    }

    pub(super) fn reset_sidebar_order(
        &self,
        request: ResetSidebarOrderRequest,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            match request {
                ResetSidebarOrderRequest::Workspaces => {
                    state.sidebar.workspaces = workspace_ids(&state, false)
                }
                ResetSidebarOrderRequest::Sessions { group } => {
                    let group = workspace(&group);
                    ensure_workspace(&state, &group)?;
                    let ids = session_ids(&state, &group, false);
                    state.sidebar.sessions.insert(group, ids);
                }
            }
            Ok(())
        })
    }
}
