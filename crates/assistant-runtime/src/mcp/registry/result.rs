//! 远端结果到规范 Tool Result 的有界转换；不管理目录或连接。

use agent_types::{ToolResultContent, ToolResultPart};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{McpCallFailure, McpCallFailureKind, McpCallProjection, cancelled, encoded_len};
use crate::mcp::{
    McpImageMaterializationFailure, McpImageMaterializer, McpRawCallResult, McpRawContent,
    schema::{CompiledMcpTool, McpSchemaEngine},
};

const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_IMAGE_COUNT: usize = 8;
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

pub(super) async fn project_result(
    raw: McpRawCallResult,
    tool: &CompiledMcpTool,
    schema: &McpSchemaEngine,
    image_directory: Option<&str>,
    image_materializer: &dyn McpImageMaterializer,
    cancellation: &CancellationToken,
) -> Result<McpCallProjection, McpCallFailure> {
    let mut parts = Vec::new();
    let mut text_bytes = 0usize;
    let mut json_bytes = 0usize;
    let mut image_count = 0usize;
    let mut image_bytes = 0usize;
    for block in raw.content {
        match block {
            McpRawContent::Text { text } => {
                text_bytes = text_bytes.saturating_add(text.len());
                if text_bytes > MAX_TEXT_BYTES {
                    return Err(result_limit());
                }
                parts.push(ToolResultPart::text(text));
            }
            McpRawContent::Image {
                data_base64,
                media_type,
            } => {
                let Some(directory) = image_directory else {
                    return Err(unsupported_result());
                };
                image_count = image_count.saturating_add(1);
                if image_count > MAX_IMAGE_COUNT {
                    return Err(result_limit());
                }
                let bytes = BASE64
                    .decode(data_base64)
                    .map_err(|_| unsupported_result())?;
                image_bytes = image_bytes.saturating_add(bytes.len());
                if image_bytes > MAX_IMAGE_BYTES {
                    return Err(result_limit());
                }
                let image = image_materializer
                    .materialize(directory, &media_type, &bytes, cancellation)
                    .await
                    .map_err(|failure| match failure {
                        McpImageMaterializationFailure::Cancelled => cancelled(true),
                        McpImageMaterializationFailure::TooLarge => result_limit(),
                        McpImageMaterializationFailure::Unsupported
                        | McpImageMaterializationFailure::Failed => unsupported_result(),
                    })?;
                parts.push(ToolResultPart::image(image));
            }
            McpRawContent::ResourceLink {
                uri,
                name,
                title,
                description,
                media_type,
                size,
            } => {
                let value = json!({
                    "type": "resource_link",
                    "uri": uri,
                    "name": name,
                    "title": title,
                    "description": description,
                    "mime_type": media_type,
                    "size": size,
                });
                json_bytes = json_bytes.saturating_add(encoded_len(&value));
                if json_bytes > MAX_JSON_BYTES {
                    return Err(result_limit());
                }
                parts.push(ToolResultPart::json(value));
            }
            McpRawContent::EmbeddedText {
                uri,
                media_type,
                text,
            } => {
                let rendered = format!(
                    "MCP embedded text resource\nURI: {uri}\nMIME: {}\n---\n{text}",
                    media_type.as_deref().unwrap_or("text/plain")
                );
                text_bytes = text_bytes.saturating_add(rendered.len());
                if text_bytes > MAX_TEXT_BYTES {
                    return Err(result_limit());
                }
                parts.push(ToolResultPart::text(rendered));
            }
            McpRawContent::Audio | McpRawContent::EmbeddedBlob | McpRawContent::Unsupported => {
                return Err(unsupported_result());
            }
        }
    }
    if let Some(structured) = raw.structured_content {
        json_bytes = json_bytes.saturating_add(encoded_len(&structured));
        if json_bytes > MAX_JSON_BYTES {
            return Err(result_limit());
        }
        if tool.has_output_schema() {
            tool.validate_output(schema, structured.clone())
                .await
                .map_err(|failure| McpCallFailure {
                    kind: McpCallFailureKind::UnsupportedResult,
                    instance_path: Some(failure.instance_path),
                    keyword: Some(failure.keyword),
                    remote_may_have_executed: true,
                })?;
        }
        parts.push(ToolResultPart::json(structured));
    }
    if parts.is_empty() {
        parts.push(ToolResultPart::text(
            "MCP tool returned no content.".to_owned(),
        ));
    }
    let content = ToolResultContent::parts(parts).map_err(|_| unsupported_result())?;
    Ok(McpCallProjection {
        content,
        is_error: raw.is_error,
        remote_may_have_executed: true,
    })
}

fn unsupported_result() -> McpCallFailure {
    McpCallFailure {
        kind: McpCallFailureKind::UnsupportedResult,
        instance_path: None,
        keyword: None,
        remote_may_have_executed: true,
    }
}

fn result_limit() -> McpCallFailure {
    McpCallFailure {
        kind: McpCallFailureKind::ResultLimit,
        instance_path: None,
        keyword: None,
        remote_may_have_executed: true,
    }
}
