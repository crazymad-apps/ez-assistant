//! MCP Apps 的连接协商及单次资源转换；不代理 HTTP URL，不解释用户权限。

use assistant_protocol::{McpUiResource, McpUiResourceSummary};
use assistant_runtime::McpUiResourcePage;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rmcp::model::{ClientInfo, ReadResourceRequestParams};

use super::*;

const UI_MIME: &str = "text/html;profile=mcp-app";
const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;

pub(super) fn client_info() -> ClientInfo {
    let mut info = ClientInfo::default();
    info.capabilities.extensions = Some(BTreeMap::from([(
        "io.modelcontextprotocol/ui".into(),
        serde_json::Map::from_iter([("mimeTypes".into(), serde_json::json!([UI_MIME]))]),
    )]));
    info
}

impl HostMcpConnection {
    pub(super) async fn list_ui_page(
        &self,
        cursor: Option<String>,
        cancellation: CancellationToken,
    ) -> Result<McpUiResourcePage, McpConnectionError> {
        let service = self.service.read().await;
        let service = service.as_ref().ok_or_else(resource_error)?;
        if service
            .peer_info()
            .is_none_or(|info| info.capabilities.resources.is_none())
        {
            return Ok(McpUiResourcePage {
                resources: Vec::new(),
                next_cursor: None,
            });
        }
        let params =
            cursor.map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
        let result = tokio::select! {
            () = cancellation.cancelled() => return Err(cancelled_error()),
            result = service.list_resources(params) => result.map_err(|_| resource_error())?,
        };
        if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > MAX_MESSAGE_BYTES) {
            return Err(resource_error());
        }
        Ok(McpUiResourcePage {
            resources: result
                .resources
                .into_iter()
                .filter(|resource| {
                    resource.uri.starts_with("ui://")
                        && resource.mime_type.as_deref() == Some(UI_MIME)
                })
                .map(|resource| {
                    let entries = resource
                        .meta
                        .as_ref()
                        .and_then(|m| m.get("ez-assistant/entries"))
                        .map(|v| {
                            if serde_json::to_vec(v).map_or(true, |b| b.len() > 64 * 1024) {
                                return Err(resource_error());
                            }
                            serde_json::from_value::<Vec<assistant_protocol::PluginUiEntry>>(
                                v.clone(),
                            )
                            .map_err(|_| resource_error())
                        })
                        .transpose()?
                        .unwrap_or_default();
                    if entries.len() > 128 {
                        return Err(resource_error());
                    }
                    Ok(McpUiResourceSummary {
                        entries,
                        uri: resource.uri,
                        name: resource.name,
                        title: resource.title,
                    })
                })
                .collect::<Result<Vec<_>, McpConnectionError>>()?,
            next_cursor: result.next_cursor,
        })
    }

    pub(super) async fn read_ui(
        &self,
        uri: String,
        cancellation: CancellationToken,
    ) -> Result<McpUiResource, McpConnectionError> {
        if !uri.starts_with("ui://") || uri.len() > 1024 {
            return Err(resource_error());
        }
        let service = self.service.read().await;
        let service = service.as_ref().ok_or_else(resource_error)?;
        let result = tokio::select! {
            () = cancellation.cancelled() => return Err(cancelled_error()),
            result = service.read_resource(ReadResourceRequestParams::new(uri.clone())) => result.map_err(|_| resource_error())?,
        };
        if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > MAX_MESSAGE_BYTES)
            || result.contents.len() != 1
        {
            return Err(resource_error());
        }
        let content = result
            .contents
            .into_iter()
            .next()
            .ok_or_else(resource_error)?;
        let (returned_uri, mime, html, meta) = match content {
            ResourceContents::TextResourceContents {
                uri,
                mime_type,
                text,
                meta,
            } => (uri, mime_type, text, meta),
            ResourceContents::BlobResourceContents {
                uri,
                mime_type,
                blob,
                meta,
            } => {
                let bytes = STANDARD.decode(blob).map_err(|_| resource_error())?;
                (
                    uri,
                    mime_type,
                    String::from_utf8(bytes).map_err(|_| resource_error())?,
                    meta,
                )
            }
            _ => return Err(resource_error()),
        };
        if returned_uri != uri || mime.as_deref() != Some(UI_MIME) || html.len() > MAX_HTML_BYTES {
            return Err(resource_error());
        }
        // 只投影资源自身 ui 配置；工具结果的私有 _meta 不混入此目录或日志。
        let ui = meta.and_then(|meta| meta.get("ui").cloned());
        if ui.as_ref().is_some_and(|ui| {
            !ui.is_object() || serde_json::to_vec(ui).map_or(true, |bytes| bytes.len() > 64 * 1024)
        }) {
            return Err(resource_error());
        }
        let ui = ui
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| resource_error())?;
        Ok(McpUiResource { uri, html, ui })
    }
}

fn resource_error() -> McpConnectionError {
    connection_error(
        McpConnectionFailureKind::UnsupportedExtension,
        "MCP UI 资源不可用、格式无效或超过限制",
    )
}
