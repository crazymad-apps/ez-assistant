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
    let ui_result = tool
        .definition
        .ui_resource_uri()
        .and_then(|_| ui_result(&raw));
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
                // MCP 的兼容文本可能只是 structuredContent 的 JSON 副本；
                // 按值相等时只保留后续结构化结果，独立说明与其他内容仍按原顺序保留。
                if raw.structured_content.as_ref().is_some_and(|structured| {
                    serde_json::from_str::<serde_json::Value>(&text)
                        .is_ok_and(|value| value == *structured)
                }) {
                    continue;
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
        validate_output(tool, schema, &structured).await?;
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
        ui_result,
    })
}

pub(super) async fn project_app_result(
    raw: McpRawCallResult,
    tool: &CompiledMcpTool,
    schema: &McpSchemaEngine,
) -> Result<serde_json::Value, McpCallFailure> {
    let result = ui_result(&raw).ok_or_else(result_limit)?;
    if result["content"]
        .as_array()
        .is_none_or(|items| items.len() != raw.content.len())
    {
        return Err(unsupported_result());
    }
    if let Some(value) = &raw.structured_content {
        validate_output(tool, schema, value).await?;
    }
    Ok(result)
}

async fn validate_output(
    tool: &CompiledMcpTool,
    schema: &McpSchemaEngine,
    value: &serde_json::Value,
) -> Result<(), McpCallFailure> {
    if tool.has_output_schema() {
        tool.validate_output(schema, value.clone())
            .await
            .map_err(|failure| McpCallFailure {
                kind: McpCallFailureKind::UnsupportedResult,
                instance_path: Some(failure.instance_path),
                keyword: Some(failure.keyword),
                remote_may_have_executed: true,
            })?;
    }
    Ok(())
}

fn ui_result(raw: &McpRawCallResult) -> Option<serde_json::Value> {
    let budget = raw
        .content
        .iter()
        .map(|block| match block {
            McpRawContent::Text { text } => text.len(),
            McpRawContent::Image { data_base64, .. } => data_base64.len(),
            McpRawContent::EmbeddedText { text, .. } => text.len(),
            _ => 1024,
        })
        .sum::<usize>()
        + raw.meta.as_ref().map_or(0, encoded_len)
        + raw.structured_content.as_ref().map_or(0, encoded_len);
    if budget > 256 * 1024 {
        return None;
    }
    let content = raw.content.iter().filter_map(|block| {
        let value = match block {
            McpRawContent::Text { text } => json!({"type":"text","text":text}),
            McpRawContent::Image { data_base64, media_type } => json!({"type":"image","data":data_base64,"mimeType":media_type}),
            McpRawContent::ResourceLink { uri, name, title, description, media_type, size } => {
                let mut link = json!({"type":"resource_link","uri":uri,"name":name,"title":title,"description":description,"mimeType":media_type,"size":size});
                // SDK 的可选字段必须缺省；null 不是合法的 MCP resource link 字段值。
                link.as_object_mut()?.retain(|_, value| !value.is_null());
                link
            }
            McpRawContent::EmbeddedText { uri, media_type, text } => {
                let mut resource = json!({"uri":uri,"text":text});
                if let Some(media_type) = media_type {
                    resource["mimeType"] = json!(media_type);
                }
                json!({"type":"resource","resource":resource})
            }
            _ => return None,
        };
        // 不清理私有 _meta 或 structuredContent 内部的 null，原始业务值完整保留。
        Some(value)
    }).collect::<Vec<_>>();
    let mut value = json!({"content":content,"isError":raw.is_error});
    if let Some(structured) = &raw.structured_content {
        value["structuredContent"] = structured.clone();
    }
    if let Some(meta) = &raw.meta {
        value["_meta"] = meta.clone();
    }
    (encoded_len(&value) <= 256 * 1024).then_some(value)
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
