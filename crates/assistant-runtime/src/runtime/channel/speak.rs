//! 绑定活动 Run 与渠道投递端口的 Runtime 私有 `speak` 工具。

use std::sync::Arc;

use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};
use agent_types::{ToolCallId, ToolName};
use assistant_protocol::RunId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::resolve_output_cycle_deliveries;
use crate::{
    ChannelOutputDispatchError, ChannelOutputDispatcher, ChannelSpeechSegment, RuntimeError,
    RuntimeResult, session::SessionController,
};

/// 单次正文有界；Runtime 在投递前按标点组织为最多 60 字符的独立 TTS 片段。
pub(crate) const MAX_SPEAK_TEXT_CHARS: usize = 1200;
const MAX_SPEAK_SEGMENT_CHARS: usize = 60;
/// 配额跨同一 Run 输出周期内的所有 AgentExecution 累计。
pub(crate) const MAX_SPEAK_SEGMENTS_PER_OUTPUT_CYCLE: usize = 20;
const SPEAK_TOOL_NAME: &str = "speak";
const SPEAK_TOOL_DESCRIPTION: &str = "Queue plain spoken text for the current output cycle, at most 1200 Unicode characters per call, without Markdown. Runtime automatically splits it at punctuation into ordered TTS segments of at most 60 Unicode characters. The output cycle may queue at most 20 TTS segments across all calls; compress and prioritize if needed. Calls execute sequentially. Never repeat text already accepted, including an accepted prefix reported by a failed call.";

/// `speak` 私有工具的严格输入；正文不会自动从 Assistant Message 截断生成。
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::runtime) struct SpeakInput {
    /// 不超过 1200 字符的播报正文；Runtime 自动切段，每个输出周期最多 20 段。
    text: String,
}

/// 一段播报是否已经被 Host 队列接受。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(in crate::runtime) struct SpeakOutput {
    accepted: bool,
}

/// 只有 Runtime 私有 `speak` 工具能构造此 facts，不进入用户权限规则。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SpeakAuthorizationFacts;

/// 绑定到某个活动 Run 的 `speak` 工具实例。
///
/// 每次执行先校验 Runtime 输出周期并解析目标，Host 接受片段后才计入周期配额。这样被用户
/// 取消或因渠道不可用而未入队的调用不会被当成已经完成的播报。
pub(in crate::runtime) struct SpeakTool {
    session: Arc<SessionController>,
    run_id: RunId,
    dispatcher: Arc<dyn ChannelOutputDispatcher>,
}

impl SpeakTool {
    pub(in crate::runtime) fn new(
        session: Arc<SessionController>,
        run_id: RunId,
        dispatcher: Arc<dyn ChannelOutputDispatcher>,
    ) -> Self {
        Self {
            session,
            run_id,
            dispatcher,
        }
    }
}

impl Tool for SpeakTool {
    type Input = SpeakInput;
    type ResolvedInput = String;
    type Output = SpeakOutput;

    fn name(&self) -> ToolName {
        ToolName::new(SPEAK_TOOL_NAME).expect("static tool name is valid")
    }

    fn description(&self) -> String {
        SPEAK_TOOL_DESCRIPTION.to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let text = validate_speak_text(input.text).map_err(ToolError::invalid_input)?;
        let semantic_arguments = serde_json::json!({"text": text});
        Ok(ToolResolution::with_facts(
            text,
            SpeakAuthorizationFacts,
            semantic_arguments,
        ))
    }

    fn execute<'a>(
        &'a self,
        text: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            let call_id = context.call_id().cloned().ok_or_else(|| {
                ToolError::execution("the speak tool call identity is unavailable")
            })?;
            let segments = split_speak_text(&text);
            // 整次调用先核对剩余配额，避免只排入一部分后才发现超过上限。
            prepare_speech_segment(&self.session, &self.run_id, segments.len())
                .map_err(|_| ToolError::execution("the active output cycle has insufficient speech capacity; shorten this unqueued text"))?;
            let multiple = segments.len() > 1;
            let mut queued_chars = 0;
            for (index, text) in segments.into_iter().enumerate() {
                if context.cancellation.is_cancelled() {
                    mark_speech_cancelled(&self.session, &self.run_id).map_err(|_| {
                        ToolError::execution("the active output cycle is unavailable")
                    })?;
                    return Ok(SpeakOutput { accepted: false });
                }
                // 子段只派生 Host 投递身份；规范 Tool Call/Result 仍是一对。
                let segment_id = if multiple {
                    ToolCallId::new(format!("{}-s{}", call_id.as_str(), index + 1))
                        .map_err(|_| ToolError::execution("invalid speech segment identity"))?
                } else {
                    call_id.clone()
                };
                let deliveries = prepare_speech_segment(&self.session, &self.run_id, 1)
                    .map_err(|_| ToolError::execution(format!("speech stopped after {queued_chars} Unicode characters were queued; do not repeat that prefix")))?;
                let count = text.chars().count();
                let dispatch = self
                    .dispatcher
                    .dispatch_speech(ChannelSpeechSegment {
                        session_id: self.session.id().clone(),
                        run_id: self.run_id.clone(),
                        segment_id,
                        text,
                        deliveries,
                        cancellation: context.cancellation.clone(),
                    })
                    .await;
                match dispatch {
                    Ok(()) => {}
                    Err(ChannelOutputDispatchError::Cancelled) => {
                        mark_speech_cancelled(&self.session, &self.run_id).map_err(|_| {
                            ToolError::execution("the active output cycle is unavailable")
                        })?;
                        return Ok(SpeakOutput { accepted: false });
                    }
                    Err(ChannelOutputDispatchError::Unavailable) => {
                        return Err(ToolError::execution(format!(
                            "speech stopped after {queued_chars} Unicode characters were queued; do not repeat that prefix; the remaining spoken text could not be queued"
                        )));
                    }
                }
                // 每次 await 后重新核对 Run，旧投递不得改写后继输出周期。
                mark_speech_accepted(&self.session, &self.run_id)
                    .map_err(|_| ToolError::execution("the active output cycle is unavailable"))?;
                queued_chars += count;
            }
            Ok(SpeakOutput { accepted: true })
        })
    }
}

/// 在调用 Host 前完成只读准入与目标解析。
///
/// 此处持有 Session 状态锁时不执行异步 I/O，也不预占片段配额；只有 Host 真正接受片段后，
/// [`mark_speech_accepted`] 才修改输出周期。目标会随本次调用一起交给 Host，后续托管切换只影响
/// 下一次解析，不改写已经进入播放链路的片段。
fn prepare_speech_segment(
    session: &SessionController,
    run_id: &RunId,
    segment_count: usize,
) -> RuntimeResult<Vec<crate::ResolvedChannelDelivery>> {
    let state = session.lock_state()?;
    if state
        .active_run
        .as_ref()
        .is_none_or(|active| active.run_id != *run_id)
    {
        return Err(RuntimeError::InvalidRequest {
            reason: "speak does not belong to the active run",
        });
    }
    let cycle = state
        .output_cycle
        .as_ref()
        .ok_or(RuntimeError::InvalidRequest {
            reason: "session has no active output cycle",
        })?;
    if cycle.speech_segment_count + segment_count > MAX_SPEAK_SEGMENTS_PER_OUTPUT_CYCLE {
        return Err(RuntimeError::InvalidRequest {
            reason: "speak segment limit reached",
        });
    }
    resolve_output_cycle_deliveries(&state)
}

/// 在 Host 已接受片段后提交 Runtime 的输出周期投影。
///
/// Host 调用期间没有持有 Session 锁，因此这里必须重新验证 Run、周期和上限。验证失败时不会把
/// 迟到结果记到新的周期；本函数也不持久化音频、播放位置或播报正文。
fn mark_speech_accepted(session: &SessionController, run_id: &RunId) -> RuntimeResult<()> {
    let mut state = session.lock_state()?;
    if state
        .active_run
        .as_ref()
        .is_none_or(|active| active.run_id != *run_id)
    {
        return Err(RuntimeError::InvalidRequest {
            reason: "speak does not belong to the active run",
        });
    }
    let cycle = state
        .output_cycle
        .as_mut()
        .ok_or(RuntimeError::InvalidRequest {
            reason: "session has no active output cycle",
        })?;
    if cycle.speech_segment_count >= MAX_SPEAK_SEGMENTS_PER_OUTPUT_CYCLE {
        return Err(RuntimeError::InvalidRequest {
            reason: "speak segment limit reached",
        });
    }
    cycle.has_speech = true;
    cycle.speech_segment_count += 1;
    Ok(())
}

/// 记录 Host 已正常取消本周期播报，避免结束门禁把用户接管误判为漏播。
fn mark_speech_cancelled(session: &SessionController, run_id: &RunId) -> RuntimeResult<()> {
    let mut state = session.lock_state()?;
    if state
        .active_run
        .as_ref()
        .is_none_or(|active| active.run_id != *run_id)
    {
        return Err(RuntimeError::InvalidRequest {
            reason: "speak does not belong to the active run",
        });
    }
    let cycle = state
        .output_cycle
        .as_mut()
        .ok_or(RuntimeError::InvalidRequest {
            reason: "session has no active output cycle",
        })?;
    cycle.speech_cancelled = true;
    Ok(())
}

/// 校验模型提供的单段朗读正文。
///
/// 限制按 Unicode 标量值计数；按标点切段由 Runtime 执行，不依赖模型遵守长度提示。
fn validate_speak_text(text: String) -> Result<String, &'static str> {
    let text = text.trim().to_owned();
    if text.is_empty() || text.chars().count() > MAX_SPEAK_TEXT_CHARS {
        return Err("speak text must contain at most 1200 Unicode characters");
    }
    if text.chars().any(char::is_control) {
        return Err("speak text must not contain control characters");
    }
    Ok(text)
}

/// 按标点组织短句；超长句按 Unicode 字符切分，保持正文内容和顺序。
fn split_speak_text(text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut count = 0;
    for token in text.split_inclusive(['。', '！', '？', '.', '!', '?', '；', ';', '，', ','])
    {
        let token_len = token.chars().count();
        if count > 0 && count + token_len > MAX_SPEAK_SEGMENT_CHARS {
            segments.push(std::mem::take(&mut current));
            count = 0;
        }
        for ch in token.chars() {
            current.push(ch);
            count += 1;
            if count == MAX_SPEAK_SEGMENT_CHARS {
                segments.push(std::mem::take(&mut current));
                count = 0;
            }
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speak_text_is_trimmed_and_rejects_control_characters() {
        assert_eq!(
            validate_speak_text("  一段自然短答。  ".to_owned()).expect("valid text"),
            "一段自然短答。"
        );
        assert!(validate_speak_text("第一行\n第二行".to_owned()).is_err());
        assert!(validate_speak_text("   ".to_owned()).is_err());
        assert!(validate_speak_text("你".repeat(MAX_SPEAK_TEXT_CHARS)).is_ok());
        assert!(validate_speak_text("你".repeat(MAX_SPEAK_TEXT_CHARS + 1)).is_err());
    }

    #[test]
    fn automatic_segments_preserve_unicode_and_punctuation() {
        for text in [
            "你".repeat(129),
            "你好，世界。Hello!".repeat(20),
            "🙂".repeat(121),
        ] {
            let parts = split_speak_text(&text);
            assert_eq!(parts.concat(), text);
            assert!(
                parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().count() <= 60)
            );
        }
        let english = format!("{}.{}.", "a".repeat(39), "b".repeat(39));
        assert_eq!(
            split_speak_text(&english)
                .iter()
                .map(|p| p.chars().count())
                .collect::<Vec<_>>(),
            vec![40, 40]
        );
        let text = format!("{}，{}。", "甲".repeat(39), "乙".repeat(39));
        assert_eq!(
            split_speak_text(&text),
            vec![
                format!("{}，", "甲".repeat(39)),
                format!("{}。", "乙".repeat(39))
            ]
        );
    }

    #[test]
    fn speak_description_exposes_per_cycle_segment_limit() {
        assert!(SPEAK_TOOL_DESCRIPTION.contains("at most 20 TTS segments"));
        assert!(SPEAK_TOOL_DESCRIPTION.contains("at most 60 Unicode characters"));
    }
}
