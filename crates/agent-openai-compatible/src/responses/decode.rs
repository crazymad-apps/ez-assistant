//! 非流式 Responses 对象通过与 SSE 相同的 item 状态机解码，避免维护第二套聚合语义。

use agent_model::{ModelError, ModelEvent};
use serde_json::{Value, json};

use super::{ResponsesProtocolAdapter, stream::ResponsesAssembler};

pub fn decode_response(
    response: &Value,
    adapter: &ResponsesProtocolAdapter,
    configured_model: &str,
) -> Result<Vec<ModelEvent>, ModelError> {
    let mut assembler = ResponsesAssembler::new(adapter.clone(), configured_model.to_owned());
    let mut events = assembler.push(&json!({
        "type": "response.created",
        "response": response,
    }))?;
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| ModelError::Protocol("Responses output must be an array".to_owned()))?;
    for (index, item) in output.iter().enumerate() {
        let output_index = u64::try_from(index)
            .map_err(|_| ModelError::Protocol("Responses output index overflow".to_owned()))?;
        events.extend(assembler.push(&json!({
            "type": "response.output_item.added",
            "output_index": output_index,
            "item": item,
        }))?);
        events.extend(assembler.push(&json!({
            "type": "response.output_item.done",
            "output_index": output_index,
            "item": item,
        }))?);
    }
    let terminal_type = match response
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
    {
        "completed" => "response.completed",
        "incomplete" => "response.incomplete",
        "failed" => "response.failed",
        "cancelled" => "response.cancelled",
        other => {
            return Err(ModelError::Protocol(format!(
                "unsupported non-streaming Responses status `{other}`"
            )));
        }
    };
    events.extend(assembler.push(&json!({
        "type": terminal_type,
        "response": response,
    }))?);
    Ok(events)
}
