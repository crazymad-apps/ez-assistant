//! 子任务首条 User Message 的稳定构造。

use agent_types::{TextPart, UserMessage, UserPart};
use assistant_protocol::AgentVariant;

use super::tool::DelegateTaskInput;
use crate::{
    SessionExecutionEnvironment,
    run::{allocate_message_id, allocate_part_id},
};

/// 将模型提供的任务正文与 Runtime 注入项持久化为子任务的首条消息。
///
/// `task` 是子 Agent 唯一的用户正文；可选上下文、期望输出、变体和稳定目录均使用
/// `Injected` Part，避免 UI 或后续上下文处理把它们误认为用户原文。
pub(super) fn child_user_message(
    input: &DelegateTaskInput,
    variant: AgentVariant,
    environment: &SessionExecutionEnvironment,
    task_private_directory: &str,
) -> Result<UserMessage, crate::RuntimeError> {
    let mut parts = vec![UserPart::Text(TextPart {
        id: allocate_part_id()?,
        text: input.task().to_owned(),
    })];
    if let Some(context) = input.context() {
        parts.push(injected_part(format!(
            "<delegated_context>\n{context}\n</delegated_context>"
        ))?);
    }
    if let Some(expected_output) = input.expected_output() {
        parts.push(injected_part(format!(
            "<expected_output>\n{expected_output}\n</expected_output>"
        ))?);
    }
    parts.push(injected_part(
        crate::agent_variant::injection_text(variant).to_owned(),
    )?);
    parts.push(injected_part(format!(
        "<runtime_directories>\nworking_directory: {}\nsession_attachment_directory: {}\nsession_private_directory: {}\nworkspace_private_directory: {}\nchild_task_private_directory: {}\n</runtime_directories>",
        environment.working_directory,
        environment.session_attachment_directory,
        environment.session_private_directory,
        environment.workspace_private_directory.as_deref().unwrap_or("unavailable"),
        task_private_directory,
    ))?);
    Ok(UserMessage {
        id: allocate_message_id()?,
        parts,
    })
}

fn injected_part(text: String) -> Result<UserPart, crate::RuntimeError> {
    Ok(UserPart::Injected(TextPart {
        id: allocate_part_id()?,
        text,
    }))
}
