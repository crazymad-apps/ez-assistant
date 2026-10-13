//! 从可靠 Conversation 恢复请求增量；服务实例不保存可变链头，也不写入存储。
use agent_model::{ModelError, ModelRequest, PreparedModelImages};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, FinishReason,
    MAX_PROVIDER_STATE_TURN_BYTES, OpaqueProviderState,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    ResponsesProtocolAdapter,
    encode::{encode_input_from, encode_request_unchecked},
    schema::ResponsesRequest,
};

pub(super) const STATE_TYPE: &str = "responses.continuation";

/// 只随完整 AssistantMessage 保存；context_hash 覆盖当前有效 wire 前缀、响应和语义参数。
/// 重启后的比较不依赖内存指针；旧 v1 不再写入或恢复 ID。
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Continuation {
    binding: String,
    context_len: usize,
    context_hash: String,
    response_id: String,
}

pub(super) fn resume(
    binding: &str,
    request: &ModelRequest,
    images: &PreparedModelImages,
    adapter: &ResponsesProtocolAdapter,
    model: &str,
    encoded: &mut ResponsesRequest,
) -> Result<(), ModelError> {
    encoded.store = true;
    // 只考察最近的持久续接事实；不能跨过更晚的配置/路由边界捡回早期链头。
    for (index, message) in request.conversation.messages.iter().enumerate().rev() {
        let ConversationMessage::Assistant(assistant) = message else {
            continue;
        };
        let Some(state) = assistant.parts.iter().find_map(|part| match part {
            AssistantPart::ProviderState(state) if state.state_type() == STATE_TYPE => Some(state),
            _ => None,
        }) else {
            continue;
        };
        if state.provider() != &adapter.provider
            || state.protocol() != &adapter.protocol
            || state.route_fingerprint() != adapter.route_fingerprint.as_deref()
        {
            return Ok(());
        }
        // 已发布 v1 仅保留为历史，不再取得增量资格。
        if state.format_version() == 1 {
            return Ok(());
        }
        if state.format_version() != 2 || state.media_type() != "application/json" {
            return Err(ModelError::Protocol(
                "Responses continuation format is invalid".into(),
            ));
        }
        let saved: Continuation = serde_json::from_slice(state.payload()).map_err(|_| {
            ModelError::Protocol("Responses continuation payload is invalid".into())
        })?;
        if saved.binding != binding || saved.context_len != index + 1 {
            return Ok(());
        }
        let mut prefix = request.clone();
        prefix.conversation.messages.truncate(index + 1);
        if saved.context_hash != context_hash(&prefix, images, adapter, model)? {
            return Ok(());
        }
        if saved.response_id.is_empty()
            || saved.response_id.len() > 1024
            || saved.response_id.chars().any(char::is_control)
        {
            return Err(ModelError::Protocol(
                "Responses continuation ID is invalid".into(),
            ));
        }
        encoded.input = encode_input_from(request, images, adapter, index + 1)?;
        encoded.previous_response_id = Some(saved.response_id);
        return Ok(());
    }
    Ok(())
}

/// 只在 SSE 唯一终态及尾部验证完成后调用；这里生成候选元数据，不表示消息已经落账。
/// Core/Runtime 既有 Recorder 确认仍是工具副作用及下一次请求的可靠门禁。
pub(super) fn attach(
    binding: &str,
    request: &ModelRequest,
    images: &PreparedModelImages,
    model: &str,
    adapter: &ResponsesProtocolAdapter,
    message: &mut AssistantMessage,
) -> Result<(), ModelError> {
    if !matches!(
        message.finish_reason,
        FinishReason::Stop | FinishReason::ToolCalls
    ) {
        return Ok(());
    }
    let mut prefix = request.clone();
    prefix
        .conversation
        .messages
        .push(ConversationMessage::Assistant(message.clone()));
    let saved = Continuation {
        binding: binding.to_owned(),
        context_len: prefix.conversation.messages.len(),
        context_hash: context_hash(&prefix, images, adapter, model)?,
        response_id: message.id.as_str().to_owned(),
    };
    let payload = serde_json::to_vec(&saved)
        .map_err(|_| ModelError::Protocol("Responses continuation encoding failed".into()))?;
    let fingerprint = adapter
        .route_fingerprint
        .clone()
        .ok_or_else(|| ModelError::Config("Responses continuation needs a bound route".into()))?;
    let state = OpaqueProviderState::new_routed(
        adapter.provider.clone(),
        adapter.protocol.clone(),
        STATE_TYPE,
        "application/json",
        2,
        None,
        fingerprint,
        payload,
    )
    .map_err(|_| ModelError::Protocol("Responses continuation state is invalid".into()))?;
    let existing_bytes = message
        .parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::ProviderState(state) => Some(state.payload().len()),
            _ => None,
        })
        .fold(0_usize, usize::saturating_add);
    if existing_bytes.saturating_add(state.payload().len()) > MAX_PROVIDER_STATE_TURN_BYTES {
        return Err(ModelError::Protocol(
            "Responses provider state exceeds the turn limit".into(),
        ));
    }
    message.parts.push(AssistantPart::ProviderState(state));
    Ok(())
}

fn context_hash(
    request: &ModelRequest,
    images: &PreparedModelImages,
    adapter: &ResponsesProtocolAdapter,
    model: &str,
) -> Result<String, ModelError> {
    let encoded = encode_request_unchecked(request, images, adapter, model)?;
    let mut canonical = serde_json::to_value(encoded)
        .map_err(|_| ModelError::Protocol("Responses continuation encoding failed".into()))?;
    if let Some(object) = canonical.as_object_mut() {
        for key in ["store", "stream", "previous_response_id"] {
            object.remove(key);
        }
    }
    hash(&canonical)
}

fn hash(value: &impl Serialize) -> Result<String, ModelError> {
    let bytes = serde_json::to_vec(value).map_err(|_| {
        ModelError::Protocol("Responses continuation fingerprint encoding failed".into())
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
