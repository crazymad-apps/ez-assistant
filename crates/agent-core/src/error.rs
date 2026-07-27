//! 执行错误分类。

use agent_model::ModelError;
use thiserror::Error;

use crate::RecordError;

/// 一次 Agent 执行的受控终止原因。
///
/// 工具执行失败与授权 `Deny` **不是**执行错误：它们转换为错误 `ToolResult`
/// 回喂模型，驱动循环继续。预算是副作用前的硬边界（`max_steps` 模型调用前
/// 预检、`max_tool_calls` dispatch 前预检），到达即 `BudgetExceeded` 受控终止。
#[derive(Clone, Debug, Error, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "error", rename_all = "snake_case")]
pub enum ExecutionError {
    /// 模型调用失败（建立前失败或流中受控失败）。
    #[error(transparent)]
    Model(#[from] ModelError),
    /// 规范对话落账失败；阻断后续副作用。
    #[error(transparent)]
    Record(#[from] RecordError),
    /// 资源预算到达上限；由副作用前预检触发。
    #[error("execution budget exceeded ({kind:?}, limit {limit})")]
    BudgetExceeded {
        /// 触达的预算类别。
        kind: BudgetKind,
        /// 预算上限值。
        limit: u32,
    },
}

/// 资源预算类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetKind {
    /// 模型 Turn 数预算（模型调用前预检）。
    Steps,
    /// 工具调用数预算（dispatch 前预检）。
    ToolCalls,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_error_round_trips_serde() {
        let errors = vec![
            ExecutionError::Model(ModelError::Cancelled),
            ExecutionError::Model(ModelError::Provider {
                message: "upstream rejected the request".to_owned(),
                status: Some(400),
            }),
            ExecutionError::Record(RecordError {
                message: "disk is full".to_owned(),
            }),
            ExecutionError::BudgetExceeded {
                kind: BudgetKind::Steps,
                limit: 8,
            },
            ExecutionError::BudgetExceeded {
                kind: BudgetKind::ToolCalls,
                limit: 32,
            },
        ];
        for error in errors {
            let json = serde_json::to_string(&error).expect("serialize error");
            assert_eq!(
                serde_json::from_str::<ExecutionError>(&json).expect("deserialize error"),
                error
            );
        }
        // 稳定 tag：蛇形命名，模型取消这类无负载错误也能往返。
        let json = serde_json::to_value(ExecutionError::Model(ModelError::Cancelled))
            .expect("serialize error to value");
        assert_eq!(json["type"], "model");
        assert_eq!(json["error"], "Cancelled");
    }

    #[test]
    fn budget_kind_uses_snake_case() {
        assert_eq!(
            serde_json::to_string(&BudgetKind::Steps).expect("serialize kind"),
            "\"steps\""
        );
        assert_eq!(
            serde_json::from_str::<BudgetKind>("\"tool_calls\"").expect("deserialize kind"),
            BudgetKind::ToolCalls
        );
    }

    #[test]
    fn error_display_and_from_conversions() {
        let error = ExecutionError::BudgetExceeded {
            kind: BudgetKind::ToolCalls,
            limit: 16,
        };
        assert_eq!(
            error.to_string(),
            "execution budget exceeded (ToolCalls, limit 16)"
        );

        let error = ExecutionError::from(ModelError::Auth("invalid api key".to_owned()));
        assert!(matches!(error, ExecutionError::Model(_)));
        assert_eq!(
            error.to_string(),
            "model authentication failed: invalid api key"
        );

        let error = ExecutionError::from(RecordError {
            message: "disk is full".to_owned(),
        });
        assert!(matches!(error, ExecutionError::Record(_)));
    }
}
