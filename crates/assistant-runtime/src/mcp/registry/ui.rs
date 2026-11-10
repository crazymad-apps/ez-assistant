//! 复用当前连接 owner、租约和限流的只读 UI 资源入口；不执行业务工具。

use assistant_protocol::{McpServerTarget, McpUiResource, McpUiResourceSummary};

use super::*;

impl McpRegistry {
    pub(crate) async fn list_ui_resources(
        &self,
        target: &McpServerTarget,
        cancellation: CancellationToken,
    ) -> RuntimeResult<Vec<McpUiResourceSummary>> {
        let state = self.ui_state(target)?;
        let _lease = state.acquire_lease().ok_or_else(ui_unavailable)?;
        let _permit = tokio::select! {
            () = cancellation.cancelled() => return Err(ui_unavailable()),
            () = state.call_cancellation.cancelled() => return Err(ui_unavailable()),
            permit = state.call_limit.clone().acquire_owned() => permit.map_err(|_| ui_unavailable())?,
        };
        let cancellation = cancellation.child_token();
        let _cancel_on_exit = cancellation.clone().drop_guard();
        let resources = tokio::select! {
            () = state.call_cancellation.cancelled() => return Err(ui_unavailable()),
            result = tokio::time::timeout(state.request_timeout, list_resources(&state, cancellation)) => {
                result.map_err(|_| ui_unavailable())??
            }
        };
        self.ensure_ui_current(target, &state)?;
        Ok(resources)
    }

    pub(crate) async fn read_ui_resource(
        &self,
        target: &McpServerTarget,
        uri: &str,
        cancellation: CancellationToken,
    ) -> RuntimeResult<McpUiResource> {
        if !uri.starts_with("ui://") || uri.len() > 1024 {
            return Err(ui_unavailable());
        }
        let state = self.ui_state(target)?;
        let _lease = state.acquire_lease().ok_or_else(ui_unavailable)?;
        let _permit = tokio::select! {
            () = cancellation.cancelled() => return Err(ui_unavailable()),
            () = state.call_cancellation.cancelled() => return Err(ui_unavailable()),
            permit = state.call_limit.clone().acquire_owned() => permit.map_err(|_| ui_unavailable())?,
        };
        let cancellation = cancellation.child_token();
        let _cancel_on_exit = cancellation.clone().drop_guard();
        // 工具绑定的卡片不强制列入 resources/list；其他 URI 必须属于当前目录。
        let read = async {
            let bound = state
                .tools
                .values()
                .any(|tool| tool.definition.ui_resource_uri() == Some(uri));
            if !bound
                && !list_resources(&state, cancellation.clone())
                    .await?
                    .iter()
                    .any(|resource| resource.uri == uri)
            {
                return Err(ui_unavailable());
            }
            state
                .connection
                .read_ui_resource(uri.to_owned(), cancellation)
                .await
                .map_err(|_| ui_unavailable())
        };
        let resource = tokio::select! {
            () = state.call_cancellation.cancelled() => return Err(ui_unavailable()),
            result = tokio::time::timeout(state.request_timeout, read) => result.map_err(|_| ui_unavailable())??,
        };
        // 异步读取期间同名连接可能已换代；旧租约只保护收尾，不授权返回旧内容。
        self.ensure_ui_current(target, &state)?;
        Ok(resource)
    }

    fn ui_state(&self, target: &McpServerTarget) -> RuntimeResult<Arc<McpServerState>> {
        let state = self
            .current(&target.server_key)
            .map_err(|_| ui_unavailable())?
            .ok_or_else(ui_unavailable)?;
        if state.source != target.source || state.retired.load(Ordering::Acquire) {
            return Err(ui_unavailable());
        }
        Ok(state)
    }

    fn ensure_ui_current(
        &self,
        target: &McpServerTarget,
        expected: &Arc<McpServerState>,
    ) -> RuntimeResult<()> {
        let current = self.ui_state(target)?;
        if !Arc::ptr_eq(&current, expected) {
            return Err(ui_unavailable());
        }
        Ok(())
    }
}

async fn list_resources(
    state: &McpServerState,
    cancellation: CancellationToken,
) -> RuntimeResult<Vec<McpUiResourceSummary>> {
    let mut resources = Vec::new();
    let mut cursor = None;
    let mut cursors = BTreeSet::new();
    let mut uris = BTreeSet::new();
    let mut bytes = 0usize;
    for _ in 0..MAX_CATALOG_PAGES {
        let page = state
            .connection
            .list_ui_resources_page(cursor, cancellation.clone())
            .await
            .map_err(|_| ui_unavailable())?;
        for resource in page.resources {
            bytes = bytes
                .saturating_add(
                    serde_json::to_vec(&resource.entries)
                        .map_or(MAX_CATALOG_BYTES + 1, |b| b.len()),
                )
                .saturating_add(resource.uri.len())
                .saturating_add(resource.name.len())
                .saturating_add(resource.title.as_ref().map_or(0, String::len));
            if !resource.uri.starts_with("ui://")
                || resource.uri.len() > 1024
                || resources.len() >= MAX_TOOLS_PER_SERVER
                || bytes > MAX_CATALOG_BYTES
                || !uris.insert(resource.uri.clone())
            {
                return Err(ui_unavailable());
            }
            resources.push(resource);
        }
        let Some(next) = page.next_cursor else {
            return Ok(resources);
        };
        if next.len() > 1024 || !cursors.insert(next.clone()) {
            return Err(ui_unavailable());
        }
        cursor = Some(next);
    }
    Err(ui_unavailable())
}

fn ui_unavailable() -> RuntimeError {
    RuntimeError::InvalidRequest {
        reason: "MCP UI 资源不可用，请刷新服务后重试",
    }
}
