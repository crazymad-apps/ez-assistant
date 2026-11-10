//! OpenAI 本体显式压缩：独立 SSE 调用，完整终态后才生成既有持久化窗口。
//! 复用字节分帧而不复用普通 Turn assembler：这里只接纳压缩项，不能产生文本或工具执行事件。

use agent_model::{ModelCallContext, ModelError};
use futures_util::StreamExt;
use serde_json::{Value, json};

use super::{OpenAiResponsesService, classify_http_error, map_transport_error};
use crate::{SseParser, TransportRequest, shared::join_endpoint};

impl OpenAiResponsesService {
    pub(super) async fn compact_stream(
        &self,
        mut body: Value,
        context: &ModelCallContext,
    ) -> Result<Value, ModelError> {
        let input = body["input"]
            .as_array_mut()
            .ok_or_else(|| invalid("missing compact input"))?;
        // V2 只返回原生 compaction 项；保留用户事实，沿用旧窗口格式以继续回放。
        let retained: Vec<_> = input
            .iter()
            .filter(|item| matches!(item["role"].as_str(), Some("user" | "developer" | "system")))
            .cloned()
            .collect();
        input.push(json!({"type":"compaction_trigger"}));
        body["stream"] = json!(true);
        body["store"] = json!(false);
        let operation = async {
            let mut response = self
                .transport
                .execute(TransportRequest {
                    trace: context.trace.clone(),
                    method: "POST".into(),
                    url: join_endpoint(&self.base_url, "responses"),
                    headers: vec![
                        ("content-type".into(), "application/json".into()),
                        ("accept".into(), "text/event-stream".into()),
                        (
                            "x-codex-beta-features".into(),
                            "remote_compaction_v2".into(),
                        ),
                        (
                            "authorization".into(),
                            self.credential.authorization_header(),
                        ),
                    ],
                    body: serde_json::to_vec(&body)
                        .map_err(|_| invalid("compact request encoding failed"))?,
                })
                .await
                .map_err(map_transport_error)?;
            // 辅助操作沿用 32 MiB 总预算。失败或取消不发布部分窗口，不重试付费调用。
            let mut bytes = Vec::new();
            while let Some(chunk) = response.body.next().await {
                let chunk = chunk.map_err(map_transport_error)?;
                if bytes.len().saturating_add(chunk.len()) > 32 * 1024 * 1024 {
                    return Err(invalid(
                        "compact stream exceeds the 32 MiB transport budget",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            if !(200..300).contains(&response.status) {
                return Err(classify_http_error(
                    response.status,
                    &response.headers,
                    &bytes,
                ));
            }
            let mut parser = SseParser::new();
            let frames = parser.push(&bytes)?;
            if parser.is_incomplete() {
                return Err(invalid("compact stream ends inside an event"));
            }
            let mut terminal: Option<Value> = None;
            let mut identity: Option<String> = None;
            let mut item: Option<Value> = None;
            for frame in frames {
                if frame.data == "[DONE]" {
                    if terminal.is_none() {
                        return Err(invalid("compact stream lacks completion"));
                    }
                    continue;
                }
                if terminal.is_some() {
                    return Err(invalid("compact stream has data after completion"));
                }
                let event: Value = serde_json::from_str(&frame.data)
                    .map_err(|_| invalid("invalid compact SSE JSON"))?;
                if let Some(id) = event["response"]["id"]
                    .as_str()
                    .or(event["response_id"].as_str())
                {
                    if identity.as_deref().is_some_and(|expected| expected != id) {
                        return Err(invalid("compact response identity changed"));
                    }
                    identity = Some(id.to_owned());
                }
                match event["type"].as_str() {
                    Some(
                        "response.failed" | "response.incomplete" | "response.cancelled" | "error",
                    ) => {
                        let error = event
                            .get("response")
                            .and_then(|r| r.get("error"))
                            .or(event.get("error"))
                            .cloned()
                            .unwrap_or(event.clone());
                        return Err(stream_failure(&error));
                    }
                    Some("response.output_item.added") => {
                        if event["output_index"] != 0 || event["item"]["type"] != "compaction" {
                            return Err(invalid("compact stream introduced a noncompact item"));
                        }
                    }
                    Some("response.output_item.done") => {
                        if item.is_some() || event["output_index"] != 0 {
                            return Err(invalid("compact stream has multiple output items"));
                        }
                        item = Some(
                            event
                                .get("item")
                                .cloned()
                                .ok_or_else(|| invalid("compact item is missing"))?,
                        );
                    }
                    Some("response.completed") => {
                        let mut result = event["response"].clone();
                        let output = result["output"]
                            .as_array()
                            .ok_or_else(|| invalid("compact output is missing"))?;
                        if result["status"] != "completed"
                            || result.get("error").is_some_and(|e| !e.is_null())
                            || output.len() != 1
                            || output[0]["type"] != "compaction"
                            || item.as_ref().is_some_and(|item| item != &output[0])
                        {
                            return Err(invalid(
                                "compact stream lacks one completed compaction item",
                            ));
                        }
                        let mut window = retained.clone();
                        window.extend(output.iter().cloned());
                        result["object"] = json!("response.compaction");
                        result["output"] = json!(window);
                        terminal = Some(result);
                    }
                    Some(_) => {}
                    None => return Err(invalid("compact event type is missing")),
                }
            }
            terminal.ok_or_else(|| invalid("compact stream ended before completion"))
        };
        tokio::select! {
            biased;
            () = context.cancellation.cancelled() => Err(ModelError::Cancelled),
            result = operation => result,
        }
    }
}

fn invalid(message: &str) -> ModelError {
    ModelError::Protocol(message.into())
}

/// SSE 已建立后的失败没有 HTTP 错误状态，不能伪造 400；仅按结构化字段分类。
/// 压缩仍是单次调用，Retry-After 只传递上游事实，不在 Adapter 内自动重试。
fn stream_failure(error: &Value) -> ModelError {
    let message = error["message"]
        .as_str()
        .unwrap_or("provider reported a failed compaction")
        .to_owned();
    let retry_after_ms = error["headers"]["retry-after"]
        .as_str()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .and_then(|seconds| seconds.checked_mul(1_000));
    match error["code"].as_str().or(error["type"].as_str()) {
        Some("context_length_exceeded") => ModelError::ContextOverflow { message },
        Some("rate_limit_exceeded") => ModelError::RateLimited {
            message,
            retry_after_ms,
        },
        Some("server_error" | "service_unavailable_error") => ModelError::Unavailable {
            message,
            status: None,
            retry_after_ms,
        },
        _ => ModelError::Provider {
            message,
            status: None,
        },
    }
}
