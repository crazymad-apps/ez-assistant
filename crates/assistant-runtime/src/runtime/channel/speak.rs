//! 绑定活动 Run 与渠道投递端口的 Runtime 私有 `speak` 工具。

use std::sync::Arc;

use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};
use agent_types::ToolName;
use assistant_protocol::RunId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::resolve_output_cycle_deliveries;
use crate::{
    ChannelOutputDispatchError, ChannelOutputDispatcher, ChannelSpeechSegment, RuntimeError,
    RuntimeResult, session::SessionController,
};

/// 单次调用的字符限制用于把自然断句交给模型，同时控制单次 TTS 请求的失败面。
pub(crate) const MAX_SPEAK_TEXT_CHARS: usize = 120;
/// 配额跨同一 Run 输出周期内的所有 AgentExecution 累计。
pub(crate) const MAX_SPEAK_SEGMENTS_PER_OUTPUT_CYCLE: usize = 20;
const SPEAK_TOOL_NAME: &str = "speak";
const SPEAK_TOOL_DESCRIPTION: &str = "Queue one natural spoken segment for the current output cycle. Each call must contain at most 120 Unicode characters of plain text without Markdown and should end at a natural semantic or sentence boundary. Call this tool no more than 20 times in one output cycle. For a longer spoken reply, call this tool multiple times sequentially in the exact playback order; if 20 segments are not enough, compress and prioritize the content. Do not label the segments or split a sentence mechanically. Each accepted call is synthesized and queued immediately, so never repeat content already submitted.";

/// `speak` 私有工具的严格输入；正文不会自动从 Assistant Message 截断生成。
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::runtime) struct SpeakInput {
    /// 不超过 120 个字符的自然完整播报片段；每个输出周期最多调用 20 次。
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
            // ToolCallId 同时作为 Host 队列中的片段身份，避免再引入一套只为播报服务的序号。
            let segment_id = context.call_id().cloned().ok_or_else(|| {
                ToolError::execution("the speak tool call identity is unavailable")
            })?;
            let deliveries = prepare_speech_segment(&self.session, &self.run_id).map_err(|error| {
                match error {
                    RuntimeError::InvalidRequest {
                        reason: "speak segment limit reached",
                    } => {
                        ToolError::execution(
                            "the output cycle already queued 20 spoken segments; finish without calling speak again",
                        )
                    }
                    _ => ToolError::execution("the active output cycle is unavailable"),
                }
            })?;
            let dispatch = self
                .dispatcher
                .dispatch_speech(ChannelSpeechSegment {
                    session_id: self.session.id().clone(),
                    run_id: self.run_id.clone(),
                    segment_id,
                    text,
                    deliveries,
                    cancellation: context.cancellation,
                })
                .await;
            match dispatch {
                Ok(()) => {}
                Err(ChannelOutputDispatchError::Cancelled) => {
                    // 用户接管或输出周期取消属于正常控制流，不诱导模型把它解释成需要道歉的故障。
                    mark_speech_cancelled(&self.session, &self.run_id).map_err(|_| {
                        ToolError::execution("the active output cycle is unavailable")
                    })?;
                    return Ok(SpeakOutput { accepted: false });
                }
                Err(ChannelOutputDispatchError::Unavailable) => {
                    return Err(ToolError::execution(
                        "the spoken segment could not be queued",
                    ));
                }
            }
            // dispatch 跨越 await；再次核对活动 Run，避免迟到的 Host 结果改写后继输出周期。
            mark_speech_accepted(&self.session, &self.run_id)
                .map_err(|_| ToolError::execution("the active output cycle is unavailable"))?;
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
    if cycle.speech_segment_count >= MAX_SPEAK_SEGMENTS_PER_OUTPUT_CYCLE {
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
/// 限制按 Unicode 标量值计数而不是 UTF-8 字节数；这里仅建立可执行的硬边界，自然断句、纯文本
/// 表达和多段顺序仍由工具描述约束模型。
fn validate_speak_text(text: String) -> Result<String, &'static str> {
    let text = text.trim().to_owned();
    if text.is_empty() || text.chars().count() > MAX_SPEAK_TEXT_CHARS {
        return Err(
            "speak text must contain at most 120 Unicode characters; split longer speech into sequential calls at natural sentence boundaries",
        );
    }
    if text.chars().any(char::is_control) {
        return Err("speak text must not contain control characters");
    }
    Ok(text)
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
    fn speak_description_exposes_per_cycle_segment_limit() {
        assert!(SPEAK_TOOL_DESCRIPTION.contains("no more than 20 times"));
        assert!(SPEAK_TOOL_DESCRIPTION.contains("at most 120 Unicode characters"));
    }
}
