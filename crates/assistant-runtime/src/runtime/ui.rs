//! 应用目录和资源只读入口；不创建会话、Run、页面工具许可或第二份目录权威状态。

use super::AssistantRuntime;
use crate::{PluginUseLease, RuntimeError, RuntimeResult};
use assistant_protocol::*;

impl AssistantRuntime {
    /// 投影当前可用的插件和独立 MCP 应用；单个 MCP 来源失败保留其他来源。
    /// # Errors
    /// Runtime 不可用、目录超限或存储不可读时拒绝。
    pub async fn list_ui_apps(&self) -> RuntimeResult<ListUiAppsResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let mut bytes = 0;
        let mut result = ListUiAppsResult {
            apps: Vec::new(),
            unavailable_sources: Vec::new(),
        };
        for plugin in self.plugin.list().await?.plugins {
            if !plugin.enabled
                || plugin.package_state != PluginPackageState::Available
                || plugin.configuration.iter().any(|f| f.required && !f.is_set)
                || plugin.external_software.iter().any(|f| !f.is_set)
            {
                continue;
            }
            for entry in plugin
                .ui
                .into_iter()
                .flat_map(|ui| ui.entries)
                .filter(|e| e.kind == PluginUiEntryKind::App)
            {
                push_app(
                    &mut result,
                    &mut bytes,
                    UiAppSummary {
                        target: UiAppTarget::Plugin {
                            name: plugin.name.clone(),
                            entry_id: entry.id.clone(),
                        },
                        entry,
                    },
                )?;
            }
        }
        for (key, server) in self.mcp_service.registry.projections()? {
            if matches!(server.source, McpServerSource::Plugin(_)) {
                continue;
            }
            let target = McpServerTarget {
                source: server.source,
                server_key: key,
            };
            match self
                .mcp_service
                .registry
                .list_ui_resources(&target, self.root_cancellation.child_token())
                .await
            {
                Ok(resources) => {
                    for resource in resources {
                        let entries = resource.entries;
                        let servers = vec![target.server_key.as_str().to_owned()];
                        let valid = entries.clone();
                        if !entries.is_empty()
                            && !tokio::task::spawn_blocking(move || {
                                crate::validate_plugin_ui(&PluginUi { entries: valid }, &servers)
                                    .is_ok()
                            })
                            .await
                            .unwrap_or(false)
                        {
                            result
                                .unavailable_sources
                                .push(target.server_key.as_str().to_owned());
                            continue;
                        }
                        for entry in entries
                            .into_iter()
                            .filter(|e| e.kind == PluginUiEntryKind::App)
                        {
                            if !matches!(&entry.resource,PluginUiResource::Mcp{server,uri} if server==target.server_key.as_str() && uri==&resource.uri)
                            {
                                continue;
                            }
                            push_app(
                                &mut result,
                                &mut bytes,
                                UiAppSummary {
                                    target: UiAppTarget::Mcp {
                                        target: target.clone(),
                                        resource_uri: resource.uri.clone(),
                                        entry_id: entry.id.clone(),
                                    },
                                    entry,
                                },
                            )?;
                        }
                    }
                }
                Err(_) => result
                    .unavailable_sources
                    .push(target.server_key.as_str().to_owned()),
            }
        }
        Ok(result)
    }

    /// 按当前声明校验参数并读取代码；没有读取卡片或执行后端工具的能力。
    /// # Errors
    /// 未声明应用、参数错误、来源退役或资源不可用时拒绝。
    pub async fn read_ui_app(&self, request: ReadUiAppRequest) -> RuntimeResult<ReadUiAppResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        if serde_json::to_vec(&request.arguments).map_or(true, |b| b.len() > 256 * 1024) {
            return Err(unavailable());
        }
        if let Some(UiAppScope::Session { session_id }) = &request.mount {
            self.session(session_id).await?.ensure_active()?;
        }
        let (entry, resource, identity, tool_source, icon_data_url) = match request.target {
            UiAppTarget::Plugin { name, entry_id } => {
                let (entry, lease, revision) = self.plugin.ui_entry(&name, &entry_id).await?;
                if entry.kind != PluginUiEntryKind::App {
                    return Err(unavailable());
                }
                validate_input(&entry, request.arguments).await?;
                let identity = format!("{}:{revision}:{entry_id}", lease.sha256);
                let tool_source = self.plugin_ui_tool_source(&entry, &lease, &identity);
                let resource = self.read_plugin_ui_entry_resource(&entry, &lease).await?;
                let icon = match (request.mount.is_some(), &entry.icon) {
                    (true, Some(path)) => self.plugin.read_ui_icon(&lease, path).await,
                    _ => None,
                };
                (entry, resource, identity, tool_source, icon)
            }
            UiAppTarget::Mcp {
                target,
                resource_uri,
                entry_id,
            } => {
                if matches!(target.source, McpServerSource::Plugin(_)) {
                    return Err(unavailable());
                }
                let projection = self
                    .mcp_service
                    .registry
                    .projections()?
                    .remove(&target.server_key)
                    .filter(|p| p.source == target.source)
                    .ok_or_else(unavailable)?;
                let identity = projection
                    .fingerprint
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>();
                let tool_source = self.mcp_ui_tool_source(&target, &identity);
                let resources = self
                    .mcp_service
                    .registry
                    .list_ui_resources(&target, self.root_cancellation.child_token())
                    .await?;
                let entry = resources
                    .into_iter()
                    .find(|r| r.uri == resource_uri)
                    .into_iter()
                    .flat_map(|r| r.entries)
                    .find(|e| e.id == entry_id && e.kind == PluginUiEntryKind::App)
                    .ok_or_else(unavailable)?;
                if !matches!(&entry.resource,PluginUiResource::Mcp{server,uri} if server==target.server_key.as_str() && uri==&resource_uri)
                {
                    return Err(unavailable());
                }
                let checked = entry.clone();
                let servers = vec![target.server_key.as_str().to_owned()];
                if !tokio::task::spawn_blocking(move || {
                    crate::validate_plugin_ui(
                        &PluginUi {
                            entries: vec![checked],
                        },
                        &servers,
                    )
                    .is_ok()
                })
                .await
                .unwrap_or(false)
                {
                    return Err(unavailable());
                }
                validate_input(&entry, request.arguments).await?;
                let mut resource = self
                    .mcp_service
                    .registry
                    .read_ui_resource(&target, &resource_uri, self.root_cancellation.child_token())
                    .await?;
                if !self
                    .mcp_service
                    .registry
                    .ui_source_matches(&target, &identity)
                {
                    return Err(unavailable());
                }
                resource
                    .ui
                    .get_or_insert(McpUiResourceMeta {
                        csp: None,
                        prefers_border: None,
                    })
                    .csp = Some(entry.csp.clone());
                let tool_source = tool_source
                    .filter(|_| entry.capabilities.contains(&PluginUiCapability::CallTool));
                (entry, resource, identity, tool_source, None)
            }
        };
        let mut tool_instance_id = None;
        if let (Some(source), Some(scope)) = (tool_source, request.mount) {
            let instance = crate::id::generate("app-instance").map_err(|_| unavailable())?;
            if self.mount_ui_tools(instance.clone(), source, scope, entry.title.clone())? {
                tool_instance_id = Some(instance);
            }
        }
        Ok(ReadUiAppResult {
            resource,
            entry,
            icon_data_url,
            source_identity: identity,
            tool_instance_id,
        })
    }

    pub(super) async fn read_plugin_ui_entry_resource(
        &self,
        entry: &PluginUiEntry,
        lease: &PluginUseLease,
    ) -> RuntimeResult<McpUiResource> {
        let mut resource = match &entry.resource {
            PluginUiResource::Package { path } => McpUiResource {
                uri: format!("ui://plugin/{}", entry.id),
                html: self.plugin.read_ui_html(lease, path.clone()).await?,
                ui: None,
            },
            PluginUiResource::Mcp { server, uri } => {
                self.mcp_service
                    .registry
                    .read_ui_resource(
                        &McpServerTarget {
                            source: McpServerSource::Plugin(lease.name.clone()),
                            server_key: McpServerKey::new(server).map_err(|_| unavailable())?,
                        },
                        uri,
                        self.root_cancellation.child_token(),
                    )
                    .await?
            }
        };
        resource
            .ui
            .get_or_insert(McpUiResourceMeta {
                csp: None,
                prefers_border: Some(true),
            })
            .csp = Some(entry.csp.clone());
        Ok(resource)
    }
}

async fn validate_input(
    entry: &PluginUiEntry,
    arguments: std::collections::BTreeMap<String, serde_json::Value>,
) -> RuntimeResult<()> {
    let schema = entry.input_schema.clone();
    let input = serde_json::to_value(arguments).map_err(|_| unavailable())?;
    if tokio::task::spawn_blocking(move || {
        crate::mcp::compile_schema(&schema).is_ok_and(|s| s.is_valid(&input))
    })
    .await
    .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(unavailable())
    }
}
fn unavailable() -> RuntimeError {
    RuntimeError::InvalidRequest {
        reason: "应用入口、来源或参数不可用，请重新选择",
    }
}

fn push_app(
    result: &mut ListUiAppsResult,
    bytes: &mut usize,
    app: UiAppSummary,
) -> RuntimeResult<()> {
    *bytes = bytes.saturating_add(serde_json::to_vec(&app).map_err(|_| unavailable())?.len());
    if result.apps.len() >= 512 || *bytes > 2 * 1024 * 1024 {
        return Err(unavailable());
    }
    result.apps.push(app);
    Ok(())
}
