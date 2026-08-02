//! AgentExecution 内部的通用 Guardrail 配置与确定性检测状态。
//!
//! Core 不注入默认阈值：整个配置、单个检测器都可以省略。检测器只维护当前执行内的
//! 连续序列，不访问 Runtime、文件系统或其他外部状态。

use std::num::NonZeroU32;

use agent_tools::ToolFingerprint;
use agent_types::ToolResultStatus;

/// 一次执行启用的 Guardrail 检测器集合。
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GuardrailConfig {
    /// 连续相同 resolved invocation 检测；`None` 表示关闭。
    pub repeated_invocation: Option<GuardrailCheckConfig>,
    /// 连续工具失败检测；`None` 表示关闭。
    pub consecutive_failures: Option<GuardrailCheckConfig>,
}

/// 一个已启用 Guardrail 检测器的显式配置。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GuardrailCheckConfig {
    /// 达到阈值后只观察，还是强制终止执行。
    pub mode: ActiveGuardrailMode,
    /// 连续序列触发阈值；使用非零类型排除无意义配置。
    pub threshold: NonZeroU32,
}

/// 已启用检测器达到阈值后的行为。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveGuardrailMode {
    /// 发送一次诊断事件，但不改变授权、执行或循环行为。
    Observe,
    /// 发送诊断事件，可靠结算当前批次后终止执行。
    Enforce,
}

/// Guardrail 检测类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardrailKind {
    /// 连续出现相同的 resolved invocation。
    RepeatedInvocation,
    /// 连续产生失败 ToolResult。
    ConsecutiveFailures,
}

/// 一次达到阈值的内部检测结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GuardrailTrigger {
    pub(crate) kind: GuardrailKind,
    pub(crate) mode: ActiveGuardrailMode,
    pub(crate) threshold: NonZeroU32,
    pub(crate) observed: u32,
}

/// 两个检测器在单次 AgentExecution 内的私有状态。
#[derive(Default)]
pub(crate) struct GuardrailState {
    repeated_invocation: RepeatedInvocationState,
    consecutive_failures: ConsecutiveFailuresState,
}

impl GuardrailState {
    /// 记录一个 valid invocation；同一连续序列只在首次达到阈值时返回 Trigger。
    pub(crate) fn observe_invocation(
        &mut self,
        config: Option<GuardrailCheckConfig>,
        fingerprint: &ToolFingerprint,
    ) -> Option<GuardrailTrigger> {
        let config = config?;
        self.repeated_invocation.observe(config, fingerprint)
    }

    /// invalid item 会中断重复 invocation 序列。
    pub(crate) fn reset_repeated_invocation(&mut self) {
        self.repeated_invocation.reset();
    }

    /// 记录一个最终 ToolResult；Success 重置序列，Error 递增失败计数。
    pub(crate) fn observe_result(
        &mut self,
        config: Option<GuardrailCheckConfig>,
        status: ToolResultStatus,
    ) -> Option<GuardrailTrigger> {
        let config = config?;
        self.consecutive_failures.observe(config, status)
    }
}

#[derive(Default)]
struct RepeatedInvocationState {
    last: Option<ToolFingerprint>,
    consecutive: u32,
    emitted_at_threshold: bool,
}

impl RepeatedInvocationState {
    fn observe(
        &mut self,
        config: GuardrailCheckConfig,
        fingerprint: &ToolFingerprint,
    ) -> Option<GuardrailTrigger> {
        if self.last.as_ref() == Some(fingerprint) {
            self.consecutive = self.consecutive.saturating_add(1);
        } else {
            self.last = Some(fingerprint.clone());
            self.consecutive = 1;
            self.emitted_at_threshold = false;
        }
        trigger_once(
            &mut self.emitted_at_threshold,
            self.consecutive,
            config,
            GuardrailKind::RepeatedInvocation,
        )
    }

    fn reset(&mut self) {
        self.last = None;
        self.consecutive = 0;
        self.emitted_at_threshold = false;
    }
}

#[derive(Default)]
struct ConsecutiveFailuresState {
    consecutive: u32,
    emitted_at_threshold: bool,
}

impl ConsecutiveFailuresState {
    fn observe(
        &mut self,
        config: GuardrailCheckConfig,
        status: ToolResultStatus,
    ) -> Option<GuardrailTrigger> {
        match status {
            ToolResultStatus::Success => {
                self.consecutive = 0;
                self.emitted_at_threshold = false;
                None
            }
            ToolResultStatus::Error => {
                self.consecutive = self.consecutive.saturating_add(1);
                trigger_once(
                    &mut self.emitted_at_threshold,
                    self.consecutive,
                    config,
                    GuardrailKind::ConsecutiveFailures,
                )
            }
        }
    }
}

fn trigger_once(
    emitted_at_threshold: &mut bool,
    observed: u32,
    config: GuardrailCheckConfig,
    kind: GuardrailKind,
) -> Option<GuardrailTrigger> {
    if *emitted_at_threshold || observed < config.threshold.get() {
        return None;
    }
    *emitted_at_threshold = true;
    Some(GuardrailTrigger {
        kind,
        mode: config.mode,
        threshold: config.threshold,
        observed,
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use agent_tools::{
        Dispatcher, ResolvedBatchItemRef, Tool, ToolContext, ToolError, ToolExecuteFuture,
        ToolFingerprint, ToolRegistry, ToolResolution,
    };
    use agent_types::{ToolCall, ToolCallId, ToolName, ToolResultStatus};
    use serde_json::json;

    use super::*;

    fn config(mode: ActiveGuardrailMode, threshold: u32) -> GuardrailCheckConfig {
        GuardrailCheckConfig {
            mode,
            threshold: NonZeroU32::new(threshold).expect("non-zero threshold"),
        }
    }

    struct FingerprintTool;

    impl Tool for FingerprintTool {
        type Input = serde_json::Value;
        type ResolvedInput = serde_json::Value;
        type Output = serde_json::Value;

        fn name(&self) -> ToolName {
            ToolName::new("fingerprint").expect("valid tool name")
        }

        fn description(&self) -> String {
            "fingerprint test tool".to_owned()
        }

        fn resolve(
            &self,
            input: Self::Input,
        ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
            Ok(ToolResolution::general(input))
        }

        fn execute<'a>(
            &'a self,
            input: Self::ResolvedInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, Self::Output> {
            Box::pin(std::future::ready(Ok(input)))
        }
    }

    fn fingerprint(value: i32) -> ToolFingerprint {
        let mut registry = ToolRegistry::new();
        registry
            .register(FingerprintTool)
            .expect("register fingerprint tool");
        let call = ToolCall {
            id: ToolCallId::new("call_1").expect("valid call id"),
            name: ToolName::new("fingerprint").expect("valid tool name"),
            arguments: json!({"value": value}),
        };
        let batch = Dispatcher::resolve_batch(&registry.snapshot(), &[call]);
        let Some(ResolvedBatchItemRef::Valid(invocation)) = batch.get(0) else {
            panic!("fingerprint tool resolves");
        };
        invocation.fingerprint().clone()
    }

    #[test]
    fn repeated_detector_triggers_once_and_rearms_after_reset() {
        let mut state = GuardrailState::default();
        let check = config(ActiveGuardrailMode::Observe, 2);
        let first = fingerprint(1);
        assert_eq!(state.observe_invocation(Some(check), &first), None);
        assert_eq!(
            state.observe_invocation(Some(check), &first),
            Some(GuardrailTrigger {
                kind: GuardrailKind::RepeatedInvocation,
                mode: ActiveGuardrailMode::Observe,
                threshold: check.threshold,
                observed: 2,
            })
        );
        assert_eq!(state.observe_invocation(Some(check), &first), None);

        let other = fingerprint(2);
        assert_eq!(state.observe_invocation(Some(check), &other), None);
        assert!(state.observe_invocation(Some(check), &other).is_some());
        state.reset_repeated_invocation();
        assert_eq!(state.observe_invocation(Some(check), &other), None);
    }

    #[test]
    fn failure_detector_triggers_once_and_success_rearms_it() {
        let mut state = GuardrailState::default();
        let check = config(ActiveGuardrailMode::Enforce, 2);
        assert_eq!(
            state.observe_result(Some(check), ToolResultStatus::Error),
            None
        );
        assert!(
            state
                .observe_result(Some(check), ToolResultStatus::Error)
                .is_some()
        );
        assert_eq!(
            state.observe_result(Some(check), ToolResultStatus::Error),
            None
        );
        assert_eq!(
            state.observe_result(Some(check), ToolResultStatus::Success),
            None
        );
        assert_eq!(
            state.observe_result(Some(check), ToolResultStatus::Error),
            None
        );
        assert!(
            state
                .observe_result(Some(check), ToolResultStatus::Error)
                .is_some()
        );
    }

    #[test]
    fn omitted_detector_does_not_accumulate_hidden_state() {
        let mut state = GuardrailState::default();
        let item = fingerprint(1);
        assert_eq!(state.observe_invocation(None, &item), None);
        assert_eq!(state.observe_result(None, ToolResultStatus::Error), None);
    }

    #[test]
    fn zero_threshold_is_rejected_during_deserialization() {
        let error = serde_json::from_value::<GuardrailCheckConfig>(json!({
            "mode": "observe",
            "threshold": 0,
        }))
        .expect_err("zero threshold must be rejected");
        assert!(error.to_string().contains("nonzero"));
    }
}
