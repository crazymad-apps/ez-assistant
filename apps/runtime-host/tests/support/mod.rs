#![allow(dead_code)]

use std::{
    fs,
    io::Read,
    net::TcpListener,
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use axum::{Json, Router, response::IntoResponse, routing::post};
use reqwest::blocking::Client as HttpClient;
use serde_json::{Value, json};
use tokio::sync::oneshot;

pub struct FakeProvider {
    endpoint: String,
    shutdown: Option<oneshot::Sender<()>>,
    owner: Option<thread::JoinHandle<()>>,
}

impl FakeProvider {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
        listener
            .set_nonblocking(true)
            .expect("nonblocking fake provider");
        let address = listener.local_addr().expect("fake provider address");
        let (shutdown, shutdown_receiver) = oneshot::channel();
        let owner = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("fake Provider runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .expect("fake Provider Tokio listener");
                let app = Router::new()
                    .route("/v1/chat/completions", post(provider_response))
                    .route("/v1/responses", post(responses_provider_response));
                tokio::select! {
                    result = axum::serve(listener, app) => result.expect("serve fake Provider"),
                    _ = shutdown_receiver => {}
                }
            });
        });
        Self {
            endpoint: format!("http://{address}/v1"),
            shutdown: Some(shutdown),
            owner: Some(owner),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(owner) = self.owner.take() {
            owner.join().expect("join fake provider");
        }
    }
}

async fn provider_response(Json(body): Json<Value>) -> impl IntoResponse {
    let body_text = serde_json::to_string(&body).expect("serialize fake Provider request");
    let case = latest_case(&body_text);
    let current_turn_has_tool_result = current_turn_has_tool_result(&body);
    if matches!(case, "BLOCK_FOR_RESTART" | "CANCEL_CASE")
        || matches!(case, "GOAL_STOP_CASE" | "GOAL_RECOVERY_CASE")
            && !body_text.contains("GOAL_CONTINUATION_V1")
    {
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    let response = if body.get("model").and_then(Value::as_str) == Some("qwen3.8-max")
        && body
            .get("tools")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        let image_count = body_text.matches("data:image/").count();
        chat_text_response(
            "auxiliary-vision",
            if image_count == 2 {
                "LOCAL IMAGES VERIFIED"
            } else {
                "LOCAL IMAGES MISSING"
            },
        )
    } else if case == "DELEGATE_PARALLEL_CASE"
        && has_tool_definition(&body, "delegate_task")
        && !current_turn_has_tool_result
    {
        parallel_delegate_task_tool_response(tool_exchange_number(&body))
    } else if case == "DELEGATE_IMAGE_CASE"
        && has_tool_definition(&body, "delegate_task")
        && !has_tool_call_result(&body, "call-delegate-image")
    {
        let path = case_path(&body, "DELEGATE_IMAGE_CASE").expect("delegated image path");
        delegate_image_tool_response(path)
    } else if case == "DELEGATE_BLOCK_CASE"
        && has_tool_definition(&body, "delegate_task")
        && !current_turn_has_tool_result
    {
        blocking_delegate_task_tool_response(tool_exchange_number(&body))
    } else if case == "DELEGATE_CASE"
        && has_tool_definition(&body, "delegate_task")
        && !current_turn_has_tool_result
    {
        delegate_task_tool_response(tool_exchange_number(&body))
    } else if case == "GOAL_LONG_CASE" {
        goal_long_response(&body, &body_text)
    } else if case == "GOAL_BLOCK_CASE" {
        goal_block_response(&body)
    } else if case == "GOAL_RECOVERY_CASE" && body_text.contains("GOAL_CONTINUATION_V1") {
        if current_turn_has_tool_result {
            chat_text_response("goal-recovery-final", "recovered Goal complete")
        } else {
            named_tool_response(
                "goal-recovery-complete",
                "call-goal-recovery-complete",
                "update_goal",
                json!({"status":"complete","summary":"recovered safely"}),
            )
        }
    } else if case == "TOOL_CASE" && !current_turn_has_tool_result {
        directory_list_tool_response(tool_exchange_number(&body))
    } else if case == "WRITE_CASE" && !current_turn_has_tool_result {
        file_write_tool_response(tool_exchange_number(&body))
    } else if case == "FILE_REFERENCE_CASE" && !current_turn_has_tool_result {
        file_read_tool_response(
            attached_file_path(&body).expect("File References request contains a readable path"),
            tool_exchange_number(&body),
        )
    } else if case == "READ_IMAGE_CASE"
        && has_tool_definition(&body, "read_image")
        && !has_tool_call_result(&body, "call-read-image")
    {
        read_image_tool_response(case_path(&body, "READ_IMAGE_CASE").expect("read image path"))
    } else if case == "INSPECT_LOCAL_CASE"
        && has_tool_definition(&body, "inspect_images")
        && !has_tool_call_result(&body, "call-inspect-local")
    {
        inspect_images_tool_response(
            case_path(&body, "INSPECT_LOCAL_CASE")
                .expect("inspect image paths")
                .split('|')
                .map(str::trim)
                .collect(),
        )
    } else {
        let response_id = if matches!(case, "FILE_REFERENCE_CASE" | "TOOL_CASE" | "WRITE_CASE") {
            format!("text-{case}-{}", tool_exchange_number(&body))
        } else {
            format!("text-{case}")
        };
        let text = if case == "REPLACEMENT_CASE" {
            "replacement answer"
        } else if matches!(case, "TOOL_CASE" | "WRITE_CASE") {
            "tool answer"
        } else if case == "FILE_REFERENCE_CASE" && body_text.contains("attachment-tool-token-91") {
            "file tool verified"
        } else if case == "FILE_REFERENCE_CASE" {
            "file tool result missing"
        } else if case == "QUEUED_AFTER_RESTART" {
            "resumed answer"
        } else if case == "INSPECT_LOCAL_CASE" && body_text.contains("LOCAL IMAGES VERIFIED") {
            "local inspect answer"
        } else if case == "INSPECT_LOCAL_CASE" {
            "local inspect result missing"
        } else {
            "offline answer"
        };
        format!(
            "data: {{\"id\":{response_id:?},\"model\":\"offline-model\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":{text:?}}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":{response_id:?},\"model\":\"offline-model\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":120,\"completion_tokens\":20,\"total_tokens\":140,\"prompt_tokens_details\":{{\"cached_tokens\":80}}}}}}\n\ndata: [DONE]\n\n"
        )
    };
    ([("content-type", "text/event-stream")], response)
}

fn chat_text_response(response_id: &str, text: &str) -> String {
    format!(
        "data: {{\"id\":{response_id:?},\"model\":\"offline-model\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":{text:?}}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":{response_id:?},\"model\":\"offline-model\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":20,\"completion_tokens\":4,\"total_tokens\":24}}}}\n\ndata: [DONE]\n\n"
    )
}

fn goal_long_response(body: &Value, body_text: &str) -> String {
    if current_turn_has_tool_result(body) {
        let response_id = if has_tool_call_result(body, "call-goal-long-complete") {
            "goal-long-complete-final"
        } else {
            "goal-long-plan-final"
        };
        return chat_text_response(response_id, "Goal tool applied");
    }
    if !has_tool_call_result(body, "call-goal-plan") {
        return named_tool_response(
            "goal-plan",
            "call-goal-plan",
            "update_plan",
            json!({
                "objective":"complete the offline Goal lifecycle",
                "items":[{"text":"run continuations","status":"in_progress"}]
            }),
        );
    }
    let continuation_count = body_text.matches("GOAL_CONTINUATION_V1").count();
    if continuation_count >= 3 && !has_tool_call_result(body, "call-goal-long-complete") {
        named_tool_response(
            "goal-long-complete",
            "call-goal-long-complete",
            "update_goal",
            json!({"status":"complete","summary":"offline Goal completed"}),
        )
    } else {
        chat_text_response(
            &format!("goal-long-turn-{continuation_count}"),
            "continue the Goal",
        )
    }
}

fn goal_block_response(body: &Value) -> String {
    if current_turn_has_tool_result(body) {
        let response_id = if has_tool_call_result(body, "call-goal-block-resumed-complete") {
            "goal-block-complete-final"
        } else {
            "goal-block-paused-final"
        };
        return chat_text_response(response_id, "Goal state recorded");
    }
    if !has_tool_call_result(body, "call-goal-blocked") {
        named_tool_response(
            "goal-blocked",
            "call-goal-blocked",
            "update_goal",
            json!({"status":"blocked","summary":"need explicit user confirmation"}),
        )
    } else if !has_tool_call_result(body, "call-goal-block-resumed-complete") {
        named_tool_response(
            "goal-block-resumed-complete",
            "call-goal-block-resumed-complete",
            "update_goal",
            json!({"status":"complete","summary":"user resumed the Goal"}),
        )
    } else {
        chat_text_response("goal-block-complete-final", "resumed Goal complete")
    }
}

fn named_tool_response(
    response_id: &str,
    call_id: &str,
    tool_name: &str,
    arguments: Value,
) -> String {
    let arguments = serde_json::to_string(&arguments).expect("serialize fake tool arguments");
    let proposal = json!({
        "id": response_id,
        "model": "offline-model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": call_id,
                    "type": "function",
                    "function": { "name": tool_name, "arguments": arguments }
                }]
            },
            "finish_reason": null
        }]
    });
    let finish = json!({
        "id": response_id,
        "model": "offline-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        "usage": {
            "prompt_tokens": 20,
            "completion_tokens": 4,
            "total_tokens": 24,
            "prompt_tokens_details": { "cached_tokens": 8 }
        }
    });
    format!("data: {proposal}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

fn responses_function_call() -> String {
    [
        json!({
            "type":"response.created",
            "response":{"id":"resp-offline-tool","model":"offline-responses","status":"in_progress"}
        }),
        json!({
            "type":"response.output_item.added",
            "output_index":0,
            "item":{
                "id":"fc-offline","type":"function_call","call_id":"call-offline",
                "name":"list_pinned_memories","arguments":"","status":"in_progress"
            }
        }),
        json!({
            "type":"response.function_call_arguments.done",
            "item_id":"fc-offline","output_index":0,"name":"list_pinned_memories","arguments":"{}"
        }),
        json!({
            "type":"response.output_item.done",
            "output_index":0,
            "item":{
                "id":"fc-offline","type":"function_call","call_id":"call-offline",
                "name":"list_pinned_memories","arguments":"{}","status":"completed"
            }
        }),
        json!({
            "type":"response.completed",
            "response":{
                "id":"resp-offline-tool","model":"offline-responses","status":"completed",
                "usage":{"input_tokens":10,"output_tokens":3,"total_tokens":13}
            }
        }),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>()
        + "data: [DONE]\n\n"
}

async fn responses_provider_response(Json(body): Json<Value>) -> impl IntoResponse {
    let body_text = serde_json::to_string(&body).expect("serialize Responses request");
    if body_text.contains("RESPONSES_OPAQUE_REPLAY_CASE") {
        let answer = if body_text.contains("opaque-ciphertext") {
            "responses opaque replayed"
        } else {
            "responses opaque missing"
        };
        return (
            [("content-type", "text/event-stream")],
            responses_text_answer("resp-opaque-replay", "deepseek-v4-pro", answer),
        );
    }
    if body_text.contains("RESPONSES_OPAQUE_CASE") {
        return (
            [("content-type", "text/event-stream")],
            responses_opaque_answer(),
        );
    }
    let is_tool_case = body_text.contains("RESPONSES_TOOL_CASE");
    let has_function_output = body
        .get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|item| item.get("type").and_then(Value::as_str) == Some("function_call_output"));
    if is_tool_case && !has_function_output {
        return (
            [("content-type", "text/event-stream")],
            responses_function_call(),
        );
    }
    let answer = if is_tool_case {
        "responses tool answer"
    } else {
        "responses offline answer"
    };
    let response_id = if is_tool_case {
        "resp-offline-tool-answer"
    } else {
        "resp-offline"
    };
    let response = [
        json!({
            "type": "response.created",
            "response": {"id":response_id,"model":"offline-responses","status":"in_progress"}
        }),
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"id":"msg-offline","type":"message","role":"assistant","content":[]}
        }),
        json!({
            "type": "response.output_text.done",
            "item_id": "msg-offline",
            "output_index": 0,
            "content_index": 0,
            "text": answer
        }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "id":"msg-offline",
                "type":"message",
                "role":"assistant",
                "content":[{"type":"output_text","text":answer,"annotations":[]}]
            }
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id":response_id,
                "model":"offline-responses",
                "status":"completed",
                "usage":{"input_tokens":10,"output_tokens":3,"total_tokens":13}
            }
        }),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>()
        + "data: [DONE]\n\n";
    ([("content-type", "text/event-stream")], response)
}

fn responses_text_answer(response_id: &str, model: &str, answer: &str) -> String {
    [
        json!({
            "type": "response.created",
            "response": {"id":response_id,"model":model,"status":"in_progress"}
        }),
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"id":format!("msg-{response_id}"),"type":"message","role":"assistant","content":[]}
        }),
        json!({
            "type": "response.output_text.done",
            "item_id": format!("msg-{response_id}"),
            "output_index": 0,
            "content_index": 0,
            "text": answer
        }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "id":format!("msg-{response_id}"),
                "type":"message",
                "role":"assistant",
                "content":[{"type":"output_text","text":answer,"annotations":[]}]
            }
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id":response_id,"model":model,"status":"completed",
                "usage":{"input_tokens":10,"output_tokens":3,"total_tokens":13}
            }
        }),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>()
        + "data: [DONE]\n\n"
}

fn responses_opaque_answer() -> String {
    [
        json!({
            "type":"response.created",
            "response":{"id":"resp-opaque","model":"deepseek-v4-pro","status":"in_progress"}
        }),
        json!({
            "type":"response.output_item.added",
            "output_index":0,
            "item":{"id":"rs-opaque","type":"reasoning","summary":[],"content":[]}
        }),
        json!({
            "type":"response.output_item.done",
            "output_index":0,
            "item":{
                "id":"rs-opaque","type":"reasoning","status":"completed","summary":[],
                "content":[{"type":"reasoning_text","text":"opaque normalized reasoning"}],
                "encrypted_content":"opaque-ciphertext"
            }
        }),
        json!({
            "type":"response.output_item.added",
            "output_index":1,
            "item":{"id":"msg-opaque","type":"message","role":"assistant","content":[]}
        }),
        json!({
            "type":"response.output_text.done",
            "item_id":"msg-opaque","output_index":1,"content_index":0,
            "text":"responses opaque stored"
        }),
        json!({
            "type":"response.output_item.done",
            "output_index":1,
            "item":{
                "id":"msg-opaque","type":"message","role":"assistant",
                "content":[{"type":"output_text","text":"responses opaque stored","annotations":[]}]
            }
        }),
        json!({
            "type":"response.completed",
            "response":{
                "id":"resp-opaque","model":"deepseek-v4-pro","status":"completed",
                "usage":{"input_tokens":10,"output_tokens":3,"total_tokens":13}
            }
        }),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>()
        + "data: [DONE]\n\n"
}

/// 只观察最近一条 User Message 之后的消息，避免历史 Tool Result 让新的 Run
/// 被 fake Provider 误判为已经完成当前工具调用。
fn current_turn_has_tool_result(body: &Value) -> bool {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return false;
    };
    let Some(last_user) = messages
        .iter()
        .rposition(|message| message.get("role").and_then(Value::as_str) == Some("user"))
    else {
        return false;
    };
    messages[last_user + 1..]
        .iter()
        .any(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
}

fn tool_exchange_number(body: &Value) -> usize {
    body.get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        .count()
        + usize::from(!current_turn_has_tool_result(body))
}

fn has_tool_definition(body: &Value, expected_name: &str) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|tool| {
            tool.get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                == Some(expected_name)
        })
}

fn has_tool_call_result(body: &Value, expected_call_id: &str) -> bool {
    body.get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|message| {
            message.get("role").and_then(Value::as_str) == Some("tool")
                && message.get("tool_call_id").and_then(Value::as_str) == Some(expected_call_id)
        })
}

fn case_path<'a>(body: &'a Value, marker: &str) -> Option<&'a str> {
    body.get("messages")?
        .as_array()?
        .iter()
        .rev()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .filter_map(|message| message.get("content"))
        .find_map(|content| {
            content
                .as_str()
                .into_iter()
                .chain(
                    content
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|part| part.get("text").and_then(Value::as_str)),
                )
                .find_map(|text| text.split_once(marker).map(|(_, path)| path.trim()))
        })
        .filter(|path| !path.is_empty())
}

fn delegate_task_tool_response(exchange_number: usize) -> String {
    let proposal = json!({
        "id": format!("delegate-{exchange_number}"),
        "model": "offline-model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": format!("call-delegate-{exchange_number}"),
                    "type": "function",
                    "function": {
                        "name": "delegate_task",
                        "arguments": "{\"title\":\"Offline child\",\"task\":\"Return one final answer.\"}"
                    }
                }]
            },
            "finish_reason": null
        }]
    });
    let finish = json!({
        "id": format!("delegate-{exchange_number}"),
        "model": "offline-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }
    });
    format!("data: {proposal}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

fn delegate_image_tool_response(path: &str) -> String {
    let arguments = serde_json::to_string(&json!({
        "title": "Offline image child",
        "task": format!("READ_IMAGE_CASE {path}"),
    }))
    .expect("serialize delegated image arguments");
    let proposal = json!({
        "id": "delegate-image",
        "model": "offline-model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": "call-delegate-image",
                    "type": "function",
                    "function": { "name": "delegate_task", "arguments": arguments }
                }]
            },
            "finish_reason": null
        }]
    });
    let finish = json!({
        "id": "delegate-image",
        "model": "offline-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }
    });
    format!("data: {proposal}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

fn parallel_delegate_task_tool_response(exchange_number: usize) -> String {
    delegate_tool_response(
        exchange_number,
        &[
            ("Offline child A", "Return child A result."),
            ("Offline child B", "Return child B result."),
        ],
    )
}

fn blocking_delegate_task_tool_response(exchange_number: usize) -> String {
    delegate_tool_response(
        exchange_number,
        &[("Interrupted child", "BLOCK_FOR_RESTART")],
    )
}

fn delegate_tool_response(exchange_number: usize, tasks: &[(&str, &str)]) -> String {
    let tool_calls = tasks
        .iter()
        .enumerate()
        .map(|(index, (title, task))| {
            json!({
                "index": index,
                "id": format!("call-delegate-{exchange_number}-{index}"),
                "type": "function",
                "function": {
                    "name": "delegate_task",
                    "arguments": serde_json::to_string(&json!({
                        "title": title,
                        "task": task,
                    })).expect("serialize delegate arguments")
                }
            })
        })
        .collect::<Vec<_>>();
    let proposal = json!({
        "id": format!("delegate-batch-{exchange_number}"),
        "model": "offline-model",
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "tool_calls": tool_calls },
            "finish_reason": null
        }]
    });
    let finish = json!({
        "id": format!("delegate-batch-{exchange_number}"),
        "model": "offline-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }
    });
    format!("data: {proposal}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

fn directory_list_tool_response(exchange_number: usize) -> String {
    let proposal = json!({
        "id": format!("tool-{exchange_number}"),
        "model": "offline-model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": format!("call-list-directory-{exchange_number}"),
                    "type": "function",
                    "function": {
                        "name": "list_directory",
                        "arguments": "{\"path\":\".\"}"
                    }
                }]
            },
            "finish_reason": null
        }]
    });
    let finish = json!({
        "id": format!("tool-{exchange_number}"),
        "model": "offline-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }
    });
    format!("data: {proposal}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

fn file_write_tool_response(exchange_number: usize) -> String {
    let proposal = json!({
        "id": format!("write-tool-{exchange_number}"),
        "model": "offline-model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": format!("call-write-file-{exchange_number}"),
                    "type": "function",
                    "function": {
                        "name": "write_file",
                        "arguments": "{\"path\":\"default-permission-write.txt\",\"content\":\"written by default workspace permission\"}"
                    }
                }]
            },
            "finish_reason": null
        }]
    });
    let finish = json!({
        "id": format!("write-tool-{exchange_number}"),
        "model": "offline-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }
    });
    format!("data: {proposal}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

fn latest_case(body: &str) -> &'static str {
    [
        "FIRST_CASE",
        "BLOCK_FOR_RESTART",
        "QUEUED_AFTER_RESTART",
        "TOOL_CASE",
        "WRITE_CASE",
        "CANCEL_CASE",
        "REPLACEMENT_CASE",
        "FILE_REFERENCE_CASE",
        "DELEGATE_CASE",
        "DELEGATE_PARALLEL_CASE",
        "DELEGATE_BLOCK_CASE",
        "DELEGATE_IMAGE_CASE",
        "READ_IMAGE_CASE",
        "INSPECT_LOCAL_CASE",
        "GOAL_LONG_CASE",
        "GOAL_BLOCK_CASE",
        "GOAL_STOP_CASE",
        "GOAL_RECOVERY_CASE",
    ]
    .into_iter()
    .filter_map(|marker| body.rfind(marker).map(|position| (position, marker)))
    .max_by_key(|(position, _)| *position)
    .map_or("DEFAULT_CASE", |(_, marker)| marker)
}

fn attached_file_path(body: &Value) -> Option<&str> {
    body.get("messages")?
        .as_array()?
        .iter()
        .rev()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .filter_map(|message| message.get("content").and_then(Value::as_array))
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .find_map(|text| {
            let start = text.find("<path>")? + "<path>".len();
            let end = text[start..].find("</path>")? + start;
            Some(&text[start..end])
        })
}

fn file_read_tool_response(path: &str, exchange_number: usize) -> String {
    let arguments =
        serde_json::to_string(&json!({ "path": path })).expect("serialize read_file arguments");
    let proposal = json!({
        "id": format!("file-tool-{exchange_number}"),
        "model": "offline-model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": format!("call-read-file-{exchange_number}"),
                    "type": "function",
                    "function": { "name": "read_file", "arguments": arguments }
                }]
            },
            "finish_reason": null
        }]
    });
    let finish = json!({
        "id": format!("file-tool-{exchange_number}"),
        "model": "offline-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }
    });
    format!("data: {proposal}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

fn read_image_tool_response(path: &str) -> String {
    let arguments =
        serde_json::to_string(&json!({ "path": path })).expect("serialize read_image arguments");
    let proposal = json!({
        "id": "read-image-tool",
        "model": "offline-model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": "call-read-image",
                    "type": "function",
                    "function": { "name": "read_image", "arguments": arguments }
                }]
            },
            "finish_reason": null
        }]
    });
    let finish = json!({
        "id": "read-image-tool",
        "model": "offline-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }
    });
    format!("data: {proposal}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

fn inspect_images_tool_response(paths: Vec<&str>) -> String {
    let arguments = serde_json::to_string(&json!({
        "image_paths": paths,
        "goal": "Verify that both local images are present."
    }))
    .expect("serialize inspect_images arguments");
    let proposal = json!({
        "id": "inspect-local-tool",
        "model": "offline-model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": "call-inspect-local",
                    "type": "function",
                    "function": { "name": "inspect_images", "arguments": arguments }
                }]
            },
            "finish_reason": null
        }]
    });
    let finish = json!({
        "id": "inspect-local-tool",
        "model": "offline-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }
    });
    format!("data: {proposal}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

pub fn write_config(runtime_home: &Path, endpoint: &str, api_key: &str) {
    fs::create_dir_all(runtime_home).expect("create runtime home");
    let document = format!(
        r#"schema_version = 1
default_model = "fixture"

[runtime.model_transport]
connect_timeout_ms = 1000
request_timeout_ms = 10000

[models.fixture]
protocol = "chat_completions"
provider = "fixture"
endpoint = "{endpoint}"
model = "offline-model"
api_key = "{api_key}"
context_window_tokens = 8192
max_output_tokens = 4096

[models.alternate]
protocol = "chat_completions"
provider = "fixture"
endpoint = "{endpoint}"
model = "offline-alternate"
api_key = "{api_key}"
context_window_tokens = 8192
max_output_tokens = 4096
"#
    );
    fs::write(runtime_home.join("config.toml"), document).expect("write test config");
}

pub fn write_responses_config(runtime_home: &Path, endpoint: &str, api_key: &str) {
    fs::create_dir_all(runtime_home).expect("create runtime home");
    let document = format!(
        r#"schema_version = 1
default_model = "responses-fixture"

[runtime.model_transport]
connect_timeout_ms = 1000
request_timeout_ms = 10000

[models.responses-fixture]
protocol = "openai_responses"
provider = "fixture"
endpoint = "{endpoint}"
model = "offline-responses"
api_key = "{api_key}"
context_window_tokens = 8192
max_output_tokens = 4096
"#
    );
    fs::write(runtime_home.join("config.toml"), document).expect("write Responses test config");
}

pub fn write_deepseek_responses_config(runtime_home: &Path, endpoint: &str, api_key: &str) {
    fs::create_dir_all(runtime_home).expect("create runtime home");
    let document = format!(
        r#"schema_version = 1
default_model = "deepseek-responses"

[runtime.model_transport]
connect_timeout_ms = 1000
request_timeout_ms = 10000

[models.deepseek-responses]
protocol = "openai_responses"
provider = "deepseek"
endpoint = "{endpoint}"
model = "deepseek-v4-pro"
api_key = "{api_key}"
context_window_tokens = 8192
max_output_tokens = 4096
"#
    );
    fs::write(runtime_home.join("config.toml"), document)
        .expect("write DeepSeek Responses test config");
}

pub fn write_qwen_image_config(runtime_home: &Path, endpoint: &str, api_key: &str) {
    fs::create_dir_all(runtime_home).expect("create runtime home");
    let document = format!(
        r#"schema_version = 1
default_model = "qwen-image-fixture"

[runtime.model_transport]
connect_timeout_ms = 1000
request_timeout_ms = 10000

[models.qwen-image-fixture]
protocol = "openai_chat_completions"
provider = "dashscope"
endpoint = "{endpoint}"
model = "qwen3.8-max"
api_key = "{api_key}"
context_window_tokens = 8192
max_output_tokens = 4096
"#
    );
    fs::write(runtime_home.join("config.toml"), document).expect("write Qwen image test config");
}

pub fn write_auxiliary_vision_config(runtime_home: &Path, endpoint: &str, api_key: &str) {
    fs::create_dir_all(runtime_home).expect("create runtime home");
    let document = format!(
        r#"schema_version = 1
default_model = "text-fixture"

[runtime.model_transport]
connect_timeout_ms = 1000
request_timeout_ms = 10000

[agent.vision]
model_key = "vision-fixture"
timeout_ms = 10000
max_output_tokens = 1024

[models.text-fixture]
protocol = "openai_chat_completions"
provider = "fixture"
endpoint = "{endpoint}"
model = "offline-text"
api_key = "{api_key}"
context_window_tokens = 8192
max_output_tokens = 4096

[models.vision-fixture]
protocol = "openai_chat_completions"
provider = "dashscope"
endpoint = "{endpoint}"
model = "qwen3.8-max"
api_key = "{api_key}"
context_window_tokens = 8192
max_output_tokens = 4096
"#
    );
    fs::write(runtime_home.join("config.toml"), document)
        .expect("write auxiliary vision test config");
}

pub struct HostProcess {
    child: Option<Child>,
    base_url: String,
    access_token: String,
}

impl HostProcess {
    pub fn start(runtime_home: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ez-assistant-runtime"));
        command
            .arg("serve")
            .arg("--runtime-home")
            .arg(runtime_home)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn Runtime Host");
        let (base_url, access_token) = wait_until_ready(runtime_home, &mut child);
        Self {
            child: Some(child),
            base_url,
            access_token,
        }
    }

    pub fn connect(&self) -> Client {
        Client::connect(self.base_url.clone(), self.access_token.clone())
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn kill(mut self) -> Output {
        let mut child = self.child.take().expect("owned Runtime Host");
        child.kill().expect("kill Runtime Host");
        child.wait_with_output().expect("wait killed Host")
    }

    pub fn wait(mut self) -> Output {
        self.child
            .take()
            .expect("owned Runtime Host")
            .wait_with_output()
            .expect("wait Runtime Host")
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub struct Client {
    http: HttpClient,
    base_url: String,
    access_token: String,
    next_request: u64,
}

impl Client {
    fn connect(base_url: String, access_token: String) -> Self {
        Self {
            http: HttpClient::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(12))
                .build()
                .expect("HTTP client"),
            base_url,
            access_token,
            next_request: 1,
        }
    }

    pub fn runtime(&mut self, command_type: &str, payload: Value) -> Value {
        let command = json!({
            "scope": "runtime",
            "payload": {
                "type": command_type,
                "payload": payload
            }
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        let result = loop {
            match self.try_request(command.clone()) {
                Ok(result) => break result,
                Err((_status, body))
                    if body["error"]["code"] == "snapshot_busy" && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(20));
                }
                Err((status, body)) => {
                    panic!("Runtime command `{command_type}` failed ({status}): {body}");
                }
            }
        };
        assert_eq!(result["scope"], "runtime");
        assert_eq!(result["payload"]["type"], command_type);
        result["payload"]["payload"].clone()
    }

    pub fn conversation(&mut self, session_id: &str) -> Value {
        let mut cursor: Option<String> = None;
        let mut items = Vec::new();
        let mut metadata = None;

        loop {
            let result = self.runtime(
                "list_conversation_page",
                json!({
                    "owner": { "type": "main_session", "session_id": session_id },
                    "cursor": cursor,
                    "limit": 100
                }),
            );
            let page = &result["snapshot"]["value"];
            assert!(
                page["items"].is_array(),
                "list_conversation_page returned a non-product projection: {result}"
            );
            if metadata.is_none() {
                metadata = Some((page["owner"].clone(), page["generation"].clone()));
            }
            let mut older = page["items"]
                .as_array()
                .expect("Conversation page items")
                .clone();
            older.append(&mut items);
            items = older;

            if !page["has_more"].as_bool().unwrap_or(false) {
                break;
            }
            cursor = Some(
                page["previous_cursor"]
                    .as_str()
                    .expect("previous cursor when Conversation has more pages")
                    .to_owned(),
            );
        }
        let (owner, generation) = metadata.expect("at least one Conversation page");

        json!({
            "owner": owner,
            "generation": generation,
            "items": items,
            "previous_cursor": null,
            "has_more": false
        })
    }

    pub fn wait_for_status(
        &mut self,
        context: &str,
        session_id: &str,
        run_id: &str,
        expected: &[&str],
    ) -> Value {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let run = self.runtime(
                "get_run",
                json!({ "session_id": session_id, "run_id": run_id }),
            )["run"]
                .clone();
            if expected.contains(&run["status"].as_str().expect("run status")) {
                return run;
            }
            let status = run["status"].as_str().expect("run status");
            assert!(
                !matches!(status, "completed" | "failed" | "cancelled" | "interrupted"),
                "{context}: Run {run_id} reached unexpected terminal state: {run}"
            );
            assert!(
                Instant::now() < deadline,
                "{context}: Run {run_id} did not reach {expected:?}; last snapshot: {run}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn request(&mut self, command: Value) -> Value {
        match self.try_request(command) {
            Ok(result) => result,
            Err((status, body)) => panic!("Runtime command failed ({status}): {body}"),
        }
    }

    fn try_request(&mut self, command: Value) -> Result<Value, (reqwest::StatusCode, Value)> {
        let request_id = format!("request-{}", self.next_request);
        self.next_request += 1;
        let response = self
            .http
            .post(format!("{}/commands", self.base_url))
            .bearer_auth(&self.access_token)
            .json(&json!({
                "request_id": request_id,
                "command": command
            }))
            .send()
            .expect("send Runtime command");
        let status = response.status();
        let body: Value = response.json().expect("decode Runtime command response");
        if !status.is_success() {
            return Err((status, body));
        }
        assert_eq!(body["request_id"], request_id);
        Ok(body["result"].clone())
    }
}

fn wait_until_ready(runtime_home: &Path, child: &mut Child) -> (String, String) {
    let deadline = Instant::now() + Duration::from_secs(8);
    let discovery_path = runtime_home.join("run/runtime.json");
    let http = HttpClient::builder()
        .connect_timeout(Duration::from_millis(200))
        .timeout(Duration::from_millis(500))
        .build()
        .expect("readiness client");
    loop {
        if let Ok(bytes) = fs::read(&discovery_path)
            && let Ok(discovery) = serde_json::from_slice::<Value>(&bytes)
            && let (Some(base_url), Some(access_token)) = (
                discovery["address"].as_str(),
                discovery["access_token"].as_str(),
            )
            && http
                .get(format!("{base_url}/health"))
                .bearer_auth(access_token)
                .send()
                .is_ok_and(|response| response.status().is_success())
        {
            return (base_url.to_owned(), access_token.to_owned());
        }
        if let Some(status) = child.try_wait().expect("poll Runtime Host") {
            let mut stdout = String::new();
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stdout.take() {
                let _ = pipe.read_to_string(&mut stdout);
            }
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            panic!(
                "Runtime Host exited before ready: {status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
        }
        assert!(
            Instant::now() < deadline,
            "Runtime Host did not become ready"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
