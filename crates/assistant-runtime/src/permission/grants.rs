//! 后台一次性/限次预授权的领域记录与原子消费协调器。

use std::sync::Arc;

use agent_tools::ResolvedToolInvocation;
use assistant_protocol::{AgentVariant, PermissionScope, RuntimeEvent};
use tokio::sync::Mutex;

use super::{
    PermissionEffect, PermissionMatcher, PermissionPaths, PermissionRule,
    matcher::matches_rule_with_paths,
};
use crate::{RuntimeError, RuntimeResult, RuntimeStore, observation::ObservationCoordinator};

/// 持久化的预授权条目。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredPermissionGrant {
    pub grant_id: String,
    pub scope: PermissionScope,
    pub variants: Vec<AgentVariant>,
    pub matcher: PermissionMatcher,
    pub max_uses: u32,
    pub remaining_uses: u32,
    pub expires_at_ms: Option<i64>,
    pub note: String,
    pub created_at_ms: i64,
    pub consumed_at_ms: Option<i64>,
}

/// 串行匹配并原子扣减 Controller 预授权次数的运行时协调器。
pub(crate) struct PermissionGrantCoordinator {
    store: Arc<dyn RuntimeStore>,
    events: ObservationCoordinator,
    consume_gate: Mutex<()>,
}

impl PermissionGrantCoordinator {
    pub(crate) fn new(store: Arc<dyn RuntimeStore>, events: ObservationCoordinator) -> Self {
        Self {
            store,
            events,
            consume_gate: Mutex::new(()),
        }
    }

    pub(crate) async fn try_consume_controller_grant(
        &self,
        variant: AgentVariant,
        invocation: &ResolvedToolInvocation,
        paths: &PermissionPaths,
        now_ms: i64,
    ) -> RuntimeResult<Option<String>> {
        let _gate = self.consume_gate.lock().await;
        let mut grants = self
            .store
            .list_permission_grants(false)
            .await
            .map_err(|source| RuntimeError::from_store("list permission grants", source))?;
        grants.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.grant_id.cmp(&right.grant_id))
        });

        for grant in grants {
            if grant.scope != PermissionScope::Controller
                || grant.remaining_uses == 0
                || grant.expires_at_ms.is_some_and(|exp| exp <= now_ms)
            {
                continue;
            }
            let rule = PermissionRule {
                id: grant.grant_id.clone(),
                effect: PermissionEffect::Allow,
                variants: grant.variants.clone(),
                matcher: grant.matcher.clone(),
            };
            if !matches_rule_with_paths(&rule, variant, invocation, Some(paths)) {
                continue;
            }
            let consumed = self
                .store
                .consume_permission_grant(&grant.grant_id, now_ms)
                .await
                .map_err(|source| RuntimeError::from_store("consume permission grant", source))?;
            if let Some(updated) = consumed {
                let _ = self.events.send(RuntimeEvent::PermissionGrantsChanged);
                return Ok(Some(updated.grant_id));
            }
        }
        Ok(None)
    }
}
