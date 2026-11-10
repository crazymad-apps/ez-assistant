//! 页面工具生命周期。页面只持有不透明句柄，实际调用固定到当前 Runtime、挂载来源和 MCP 连接。

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use assistant_protocol::{
    ApprovalId, ConversationOwner, McpServerKey, McpServerSource, McpServerTarget,
    PluginUiCapability, PluginUiEntry, PluginUiResource, SessionId, UiAppScope, UiToolAction,
    UiToolRequest, UiToolState,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::AssistantRuntime;
use crate::{
    PermissionEffect, PermissionFileScope, PluginUseLease, RuntimeError, RuntimeResult,
    mcp::{McpAppBinding, McpAuthorizationFacts, failure_code},
};

const MAX_PAGES: usize = 512;
const MAX_REQUESTS_PER_PAGE: usize = 1024;
const MAX_IN_FLIGHT_PER_PAGE: usize = 8;
const MAX_IN_FLIGHT: usize = 64;
const PAGE_LIFETIME: Duration = Duration::from_secs(3600);
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_LEASE: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub(super) struct UiToolSource {
    connection: McpAppBinding,
    plugin: Option<PluginPage>,
    card: Option<(ConversationOwner, String)>,
}

impl UiToolSource {
    pub(super) fn for_card(mut self, owner: ConversationOwner, card_id: String) -> Self {
        self.card = Some((owner, card_id));
        self
    }
}

#[derive(Clone)]
struct PluginPage {
    name: String,
    entry_id: String,
    identity: String,
}

struct Page {
    source: UiToolSource,
    title: String,
    scope: UiAppScope,
    expires: Instant,
    cancellation: CancellationToken,
    seen: BTreeSet<String>,
    calls: BTreeMap<String, Call>,
}

struct Call {
    state: CallState,
    cancellation: CancellationToken,
    last_seen: Instant,
}

enum CallState {
    Running,
    Waiting(ApprovalId),
    Finished(UiToolState),
}

type CallKey = (String, String);

#[derive(Default)]
struct State {
    pages: BTreeMap<String, Page>,
    // 页面退役后任务仍需在此集合中保留到本地收尾，换域预检才能覆盖实际在途调用。
    active: BTreeSet<CallKey>,
}

#[derive(Default)]
pub(super) struct UiTools(Mutex<State>);

impl UiTools {
    fn state(&self) -> RuntimeResult<MutexGuard<'_, State>> {
        self.0
            .lock()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "UI tool registry",
            })
    }

    pub(super) fn active_ids(&self) -> RuntimeResult<BTreeSet<CallKey>> {
        Ok(self.state()?.active.clone())
    }

    pub(super) fn release(&self, instance_id: &str) -> RuntimeResult<()> {
        if let Some(page) = self.state()?.pages.remove(instance_id) {
            page.cancellation.cancel();
        }
        Ok(())
    }

    pub(super) fn retire_session(&self, session_id: &SessionId) {
        let Ok(mut state) = self.state() else { return };
        state.pages.retain(|_, page| {
            let owned = matches!(&page.scope, UiAppScope::Session { session_id: owner } if owner == session_id);
            if owned {
                page.cancellation.cancel();
            }
            !owned
        });
    }

    fn set_waiting(&self, key: &CallKey, approval_id: ApprovalId) -> bool {
        let Ok(mut state) = self.state() else {
            return false;
        };
        let Some(call) = state
            .pages
            .get_mut(&key.0)
            .and_then(|page| page.calls.get_mut(&key.1))
        else {
            return false;
        };
        if call.cancellation.is_cancelled() {
            return false;
        }
        call.state = CallState::Waiting(approval_id);
        true
    }

    fn finish(&self, key: &CallKey, outcome: UiToolState) {
        if let Ok(mut state) = self.state() {
            state.active.remove(key);
            if let Some(call) = state
                .pages
                .get_mut(&key.0)
                .and_then(|page| page.calls.get_mut(&key.1))
            {
                call.state = CallState::Finished(outcome);
            }
        }
    }

    async fn watch_client(
        &self,
        key: &CallKey,
        cancellation: &CancellationToken,
        done: &CancellationToken,
    ) {
        loop {
            tokio::select! {
                () = done.cancelled() => return,
                () = cancellation.cancelled() => return,
                () = tokio::time::sleep(POLL_LEASE / 3) => {}
            }
            let live = self.state().ok().is_some_and(|state| {
                state
                    .pages
                    .get(&key.0)
                    .and_then(|page| page.calls.get(&key.1))
                    .is_some_and(|call| call.last_seen.elapsed() < POLL_LEASE)
            });
            if !live {
                cancellation.cancel();
                return;
            }
        }
    }
}

struct ActiveCall {
    tools: Arc<UiTools>,
    key: CallKey,
    finished: bool,
}

impl ActiveCall {
    fn finish(mut self, outcome: UiToolState) {
        self.tools.finish(&self.key, outcome);
        self.finished = true;
    }
}

impl Drop for ActiveCall {
    fn drop(&mut self) {
        if !self.finished {
            self.tools
                .finish(&self.key, failed("ui_tool_interrupted", true));
        }
    }
}

impl AssistantRuntime {
    pub(super) fn plugin_ui_tool_source(
        &self,
        entry: &PluginUiEntry,
        lease: &PluginUseLease,
        identity: &str,
    ) -> Option<UiToolSource> {
        if !entry.capabilities.contains(&PluginUiCapability::CallTool) {
            return None;
        }
        let PluginUiResource::Mcp { server, .. } = &entry.resource else {
            return None;
        };
        let target = McpServerTarget {
            source: McpServerSource::Plugin(lease.name.clone()),
            server_key: McpServerKey::new(server).ok()?,
        };
        let connection = self
            .mcp_service
            .registry
            .bind_app(&target, None, Some(&lease.sha256))?;
        Some(UiToolSource {
            connection,
            plugin: Some(PluginPage {
                name: lease.name.clone(),
                entry_id: entry.id.clone(),
                identity: identity.to_owned(),
            }),
            card: None,
        })
    }

    pub(super) fn mcp_ui_tool_source(
        &self,
        target: &McpServerTarget,
        fingerprint: &str,
    ) -> Option<UiToolSource> {
        Some(UiToolSource {
            connection: self
                .mcp_service
                .registry
                .bind_app(target, Some(fingerprint), None)?,
            plugin: None,
            card: None,
        })
    }

    pub(super) fn mount_ui_tools(
        &self,
        instance_id: String,
        source: UiToolSource,
        scope: UiAppScope,
        title: String,
    ) -> RuntimeResult<bool> {
        if source.connection.lifetime().is_none() {
            return Ok(false);
        }
        let mut state = self.ui_tools.state()?;
        let now = Instant::now();
        state.pages.retain(|_, page| {
            let live = page.expires > now && page.source.connection.lifetime().is_some();
            if !live {
                page.cancellation.cancel();
            }
            live
        });
        if state.pages.len() >= MAX_PAGES {
            return Ok(false);
        }
        state.pages.insert(
            instance_id,
            Page {
                source,
                scope,
                title,
                expires: now + PAGE_LIFETIME,
                cancellation: self.root_cancellation.child_token(),
                seen: BTreeSet::new(),
                calls: BTreeMap::new(),
            },
        );
        Ok(true)
    }

    /// 只在接纳时取得 Runtime 操作门。任务登记后由 Runtime 等待并负责取消，HTTP 请求可立即返回。
    pub async fn ui_tool(self: &Arc<Self>, request: UiToolRequest) -> RuntimeResult<UiToolState> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let instance = request.instance_id;
        if instance.len() > 128 {
            return Err(invalid());
        }
        match request.action {
            UiToolAction::Release => {
                self.ui_tools.release(&instance)?;
                Ok(UiToolState::Acknowledged)
            }
            UiToolAction::Start {
                request_id,
                name,
                arguments,
                user_gesture,
            } => {
                if request_id.is_empty()
                    || request_id.len() > 128
                    || name.is_empty()
                    || name.len() > 128
                    || serde_json::to_vec(&arguments).map_or(true, |value| value.len() > 256 * 1024)
                {
                    return Err(invalid());
                }
                let mut state = self.ui_tools.state()?;
                let at_limit = state.active.len() >= MAX_IN_FLIGHT
                    || state
                        .pages
                        .values()
                        .map(|page| page.calls.len())
                        .sum::<usize>()
                        >= MAX_IN_FLIGHT;
                let page = state.pages.get_mut(&instance).ok_or_else(invalid)?;
                if page.expires <= Instant::now() || page.source.connection.lifetime().is_none() {
                    return Err(invalid());
                }
                if page.seen.contains(&request_id) {
                    return Ok(failed("ui_tool_duplicate_request", true));
                }
                if at_limit
                    || page.calls.len() >= MAX_IN_FLIGHT_PER_PAGE
                    || page.seen.len() >= MAX_REQUESTS_PER_PAGE
                {
                    return Ok(failed("ui_tool_request_limit", false));
                }
                let cancellation = page.cancellation.child_token();
                let (source, scope, title) =
                    (page.source.clone(), page.scope.clone(), page.title.clone());
                page.seen.insert(request_id.clone());
                page.calls.insert(
                    request_id.clone(),
                    Call {
                        state: CallState::Running,
                        cancellation: cancellation.clone(),
                        last_seen: Instant::now(),
                    },
                );
                let key = (instance, request_id);
                state.active.insert(key.clone());
                let work = ActiveCall {
                    tools: self.ui_tools.clone(),
                    key,
                    finished: false,
                };
                drop(state);
                let runtime = self.clone();
                self.tasks.spawn(
                    async move {
                        let done = CancellationToken::new();
                        let execute = async {
                            let outcome = runtime
                                .run_ui_tool(
                                    &work.key,
                                    source,
                                    scope,
                                    title,
                                    name,
                                    arguments,
                                    user_gesture.unwrap_or_default(),
                                    cancellation.clone(),
                                )
                                .await;
                            done.cancel();
                            outcome
                        };
                        let (outcome, ()) = tokio::join!(
                            execute,
                            work.tools.watch_client(&work.key, &cancellation, &done)
                        );
                        work.finish(outcome);
                    },
                    async {},
                );
                Ok(UiToolState::Pending)
            }
            UiToolAction::Poll { request_id } => {
                let mut state = self.ui_tools.state()?;
                let page = state.pages.get_mut(&instance).ok_or_else(invalid)?;
                let call = page.calls.get_mut(&request_id).ok_or_else(invalid)?;
                call.last_seen = Instant::now();
                Ok(match &call.state {
                    CallState::Running => UiToolState::Pending,
                    CallState::Waiting(id) => match self.approval_registry.app_approval(id)? {
                        Some(approval) => UiToolState::Approval { approval },
                        None => UiToolState::Pending,
                    },
                    CallState::Finished(outcome) => {
                        let outcome = outcome.clone();
                        page.calls.remove(&request_id);
                        outcome
                    }
                })
            }
            UiToolAction::Decide {
                request_id,
                approval_id,
                decision,
            } => {
                let state = self.ui_tools.state()?;
                let page = state.pages.get(&instance).ok_or_else(invalid)?;
                let call = page.calls.get(&request_id).ok_or_else(invalid)?;
                if call.cancellation.is_cancelled()
                    || page.source.connection.lifetime().is_none()
                    || !matches!(&call.state, CallState::Waiting(id) if id == &approval_id)
                {
                    return Err(invalid());
                }
                self.approval_registry.decide_app(&approval_id, decision)?;
                Ok(UiToolState::Acknowledged)
            }
            UiToolAction::Cancel { request_id } => {
                if !request_id.is_empty()
                    && request_id.len() <= 128
                    && let Some(page) = self.ui_tools.state()?.pages.get_mut(&instance)
                {
                    // Cancel 可能先于 Start 到达，保留请求 ID 以拒绝迟到的接纳。
                    if page.seen.len() < MAX_REQUESTS_PER_PAGE {
                        page.seen.insert(request_id.clone());
                    }
                    if let Some(call) = page.calls.remove(&request_id) {
                        call.cancellation.cancel();
                    }
                }
                Ok(UiToolState::Acknowledged)
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "冻结页面 owner、连接与单次 RPC 的输入"
    )]
    async fn run_ui_tool(
        &self,
        key: &CallKey,
        source: UiToolSource,
        scope: UiAppScope,
        title: String,
        name: String,
        arguments: BTreeMap<String, Value>,
        user_gesture: bool,
        cancellation: CancellationToken,
    ) -> UiToolState {
        let Some(connection_lifetime) = source.connection.lifetime() else {
            return failed("ui_tool_source_retired", false);
        };
        let registry = &self.mcp_service.registry;
        let invocation = match registry.resolve_app(
            &source.connection,
            &name,
            arguments.into_iter().collect(),
        ) {
            Ok(invocation) => invocation,
            Err(error) => return failed(failure_code(error.kind), false),
        };
        let scopes = match self.ui_tool_scopes(&scope).await {
            Ok(scopes) => scopes,
            Err(_) => return failed("ui_tool_scope_unavailable", false),
        };
        if let Err(error) = registry.validate_invocation(&invocation).await {
            return failed(failure_code(error.kind), false);
        }
        let default = if user_gesture {
            PermissionEffect::Allow
        } else {
            PermissionEffect::Ask
        };
        let effect = || {
            self.permission_coordinator.mcp_page_effect(
                &scopes,
                &invocation.server_key,
                &name,
                default,
            )
        };
        let approved = match effect() {
            Ok(PermissionEffect::Allow) => false,
            Ok(PermissionEffect::Ask) => {
                let facts = McpAuthorizationFacts {
                    invocation: invocation.clone(),
                };
                let Ok(mut approval) =
                    self.approval_registry
                        .register_app(title, scope.clone(), &facts)
                else {
                    return failed("ui_tool_approval_unavailable", false);
                };
                if !self.ui_tools.set_waiting(key, approval.id().clone()) {
                    return failed("ui_tool_cancelled", false);
                }
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return failed("ui_tool_cancelled", false),
                    () = connection_lifetime.cancelled() => return failed("ui_tool_source_retired", false),
                    decision = tokio::time::timeout(APPROVAL_TIMEOUT, approval.allowed()) => match decision {
                        Ok(true) => {}
                        Ok(false) => return failed("ui_tool_permission_denied", false),
                        Err(_) => return failed("ui_tool_approval_timeout", false),
                    },
                }
                true
            }
            Ok(PermissionEffect::Deny) | Err(_) => {
                return failed("ui_tool_permission_denied", false);
            }
        };
        if let Some(plugin) = source.plugin {
            let Ok((_, lease, revision)) =
                self.plugin.ui_entry(&plugin.name, &plugin.entry_id).await
            else {
                return failed("ui_tool_source_retired", false);
            };
            if format!("{}:{revision}:{}", lease.sha256, plugin.entry_id) != plugin.identity {
                return failed("ui_tool_source_retired", false);
            }
        }
        if let Some((owner, card_id)) = source.card
            && !self
                .store
                .get_message_card(owner, card_id)
                .await
                .is_ok_and(|card| card.is_some())
        {
            return failed("ui_tool_source_retired", false);
        }
        if self.ui_tool_scopes(&scope).await.is_err() {
            return failed("ui_tool_scope_unavailable", false);
        }
        if cancellation.is_cancelled() || connection_lifetime.is_cancelled() {
            return failed("ui_tool_cancelled", false);
        }
        if !match effect() {
            Ok(PermissionEffect::Allow) => true,
            Ok(PermissionEffect::Ask) => approved,
            Ok(PermissionEffect::Deny) | Err(_) => false,
        } {
            return failed("ui_tool_permission_denied", false);
        }
        let execute = registry.execute_app(&invocation, &cancellation);
        tokio::pin!(execute);
        let outcome = tokio::select! {
            result = &mut execute => result,
            () = connection_lifetime.cancelled() => {
                cancellation.cancel();
                execute.await
            }
        };
        match outcome {
            Ok(result) if !cancellation.is_cancelled() && !connection_lifetime.is_cancelled() => {
                UiToolState::Complete { result }
            }
            Ok(_) => failed("ui_tool_interrupted", true),
            Err(error) => failed(failure_code(error.kind), error.remote_may_have_executed),
        }
    }

    async fn ui_tool_scopes(&self, scope: &UiAppScope) -> RuntimeResult<Vec<PermissionFileScope>> {
        match scope {
            UiAppScope::Draft => Ok(vec![PermissionFileScope::Global]),
            UiAppScope::Session { session_id } => {
                let session = self.session(session_id).await?;
                session.ensure_active()?;
                Ok(session.permission_scopes())
            }
        }
    }
}

fn invalid() -> RuntimeError {
    RuntimeError::InvalidRequest {
        reason: "invalid or retired UI tool request",
    }
}

fn failed(reason: &str, remote_may_have_executed: bool) -> UiToolState {
    UiToolState::Failed {
        reason: reason.into(),
        remote_may_have_executed,
    }
}
