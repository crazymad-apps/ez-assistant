//! ask_user 只装配到主执行；Session 拥有等待事实，工具 future 不持有 mutation gate。

mod tool;
pub(crate) use tool::{AskUserTool, FeedbackAuthorizationFacts};

use crate::{RuntimeError, RuntimeResult};
use assistant_protocol::{FeedbackAnswer, FeedbackRequestSnapshot, FeedbackStatus};

/// 会话问答的可靠变更；None 只用于创建，其余转换必须匹配当前状态。
#[derive(Clone, Debug)]
pub struct FeedbackMutation {
    pub request: FeedbackRequestSnapshot,
    pub expected_status: Option<FeedbackStatus>,
}

/// 从冻结题目验证位置、基数及自由文本；不接受客户端提供的选项标签。
pub(crate) fn validate_answers(
    request: &FeedbackRequestSnapshot,
    answers: &[FeedbackAnswer],
) -> RuntimeResult<()> {
    if answers.len() != request.questions.len() {
        return Err(invalid("请回答全部问题"));
    }
    for (question, answer) in request.questions.iter().zip(answers) {
        if answer.text.len() > 32_768
            || (answer.selected_options.is_empty() && answer.text.trim().is_empty())
        {
            return Err(invalid("每题请选择选项或填写回答，文字不能超过 32768 字节"));
        }
        let indexes = answer
            .selected_options
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        if indexes.len() != answer.selected_options.len()
            || (!question.multiple && indexes.len() > 1)
            || answer
                .selected_options
                .iter()
                .any(|index| *index as usize >= question.options.len())
        {
            return Err(invalid("所选答案无效，请重新选择"));
        }
    }
    Ok(())
}

pub(crate) fn invalid(reason: &'static str) -> RuntimeError {
    RuntimeError::InvalidRequest { reason }
}

pub(crate) fn answer_message(
    request: &FeedbackRequestSnapshot,
    answers: &[FeedbackAnswer],
) -> String {
    let mut message = String::from("回复助手的问题：\n");
    for (question, answer) in request.questions.iter().zip(answers) {
        message.push_str(&format!("问题：{}\n", question.question));
        for index in &answer.selected_options {
            message.push_str(&format!(
                "选择：{}\n",
                question.options[*index as usize].label
            ));
        }
        if !answer.text.is_empty() {
            message.push_str(&format!("回答：{}\n", answer.text));
        }
    }
    message
}
