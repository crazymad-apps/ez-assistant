//! 单次 Run 冻结的解释器事实。Host 生成，Runtime 持久化；UI 只提交 ShellKind 意图。

use assistant_protocol::ShellKind;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct ShellSwitchPrevious {
    previous_shell: Option<ShellKind>,
}

/// 仅保存控制结果的历史旧值；绑定权威仍在 Session，Fork 随规范消息自然保留。
pub(crate) fn append_switch_previous(
    message: &mut agent_types::UserMessage,
    previous: Option<ShellKind>,
) -> crate::RuntimeResult<()> {
    use crate::internal_boundary::{
        InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
    };
    let text = serde_json::to_string(&ShellSwitchPrevious {
        previous_shell: previous,
    })
    .map_err(|_| crate::RuntimeError::InternalStateUnavailable {
        component: "previous shell serialization",
    })?;
    InternalBoundaryCoordinator::append(
        message,
        InternalBoundaryRequest {
            source: InternalBoundarySource::ShellSwitchPrevious,
            text,
        },
    )?;
    Ok(())
}

pub(crate) fn switch_previous(
    message: &agent_types::UserMessage,
) -> crate::RuntimeResult<Option<ShellKind>> {
    message
        .parts
        .iter()
        .find_map(|part| match part {
            agent_types::UserPart::InternalContext(part)
                if part.kind == "shell_switch_previous" =>
            {
                Some(&part.text)
            }
            _ => None,
        })
        .map(|text| {
            serde_json::from_str::<ShellSwitchPrevious>(text).map_err(|_| {
                crate::RuntimeError::InternalStateUnavailable {
                    component: "previous shell result",
                }
            })
        })
        .transpose()
        .map(|result| result.and_then(|result| result.previous_shell))
}

/// Fork 的不可变历史展示事实，不用于绑定恢复或解释器装配，也不复制源 Run。
#[derive(Serialize, Deserialize)]
pub(crate) struct InheritedShellSwitchResult {
    pub(crate) shell: ShellKind,
    pub(crate) success: bool,
    pub(crate) error: Option<assistant_protocol::RuntimeErrorInfo>,
}

impl InheritedShellSwitchResult {
    pub(crate) fn from_message(
        message: &agent_types::UserMessage,
    ) -> crate::RuntimeResult<Option<Self>> {
        message
            .parts
            .iter()
            .find_map(|part| match part {
                agent_types::UserPart::InternalContext(part)
                    if part.kind == "inherited_shell_switch_result" =>
                {
                    Some(&part.text)
                }
                _ => None,
            })
            .map(|text| {
                serde_json::from_str(text).map_err(|_| {
                    crate::RuntimeError::InternalStateUnavailable {
                        component: "inherited shell result",
                    }
                })
            })
            .transpose()
    }

    pub(crate) fn append_to(
        &self,
        message: &mut agent_types::UserMessage,
    ) -> crate::RuntimeResult<()> {
        use crate::internal_boundary::{
            InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
        };
        if Self::from_message(message)?.is_some() {
            return Ok(());
        }
        let text = serde_json::to_string(self).map_err(|_| {
            crate::RuntimeError::InternalStateUnavailable {
                component: "inherited shell result serialization",
            }
        })?;
        InternalBoundaryCoordinator::append(
            message,
            InternalBoundaryRequest {
                source: InternalBoundarySource::InheritedShellSwitchResult,
                text,
            },
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
/// Run 领取时持久化的解释器快照；启动配置和模型方言提示始终从同一份事实派生。
pub struct FrozenShellEnvironment {
    pub kind: ShellKind,
    pub operating_system: String,
    pub program: String,
    pub fixed_args: Vec<String>,
    pub command_prefix: String,
    pub dialect: String,
}

impl FrozenShellEnvironment {
    pub(crate) fn context_message(
        &self,
        cwd: &str,
    ) -> crate::RuntimeResult<agent_types::UserMessage> {
        use crate::internal_boundary::{
            InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
        };
        InternalBoundaryCoordinator::hidden_message(InternalBoundaryRequest {
            source: InternalBoundarySource::ShellEnvironment,
            text: self.render_context(cwd)?,
        })
        .map(|(message, _)| message)
    }

    /// cwd 仍来自 Session 冻结目录；转义所有动态文本，不能让路径成为内部上下文标签。
    pub fn render_context(&self, working_directory: &str) -> crate::RuntimeResult<String> {
        fn escape(value: &str) -> String {
            value
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        }
        // 完整序列化保证 kind、固定参数和前缀的任何变化都会使旧环境事实失效。
        // JSON 字符串边界同时防止路径中的换行伪装成另一个字段。
        let snapshot = serde_json::to_string(self).map_err(|_| {
            crate::RuntimeError::InternalStateUnavailable {
                component: "shell context serialization",
            }
        })?;
        let directory = serde_json::to_string(working_directory).map_err(|_| {
            crate::RuntimeError::InternalStateUnavailable {
                component: "shell directory serialization",
            }
        })?;
        Ok(format!(
            "<shell_environment>\nFrozen Shell snapshot (JSON): {}\nWorking directory (JSON): {}\nThe launcher applies fixed_args and command_prefix; provide only the command body in this dialect.\n</shell_environment>",
            escape(&snapshot),
            escape(&directory)
        ))
    }
}

/// 只认当前有效上下文中最近的结构化环境事实，普通文本或压缩摘要不能冒充它。
pub(crate) fn context_is_current(
    conversation: &agent_types::ConversationSnapshot,
    context: &agent_types::UserMessage,
) -> bool {
    use agent_types::{ConversationMessage, UserPart};
    let expected = context.parts.iter().find_map(|part| match part {
        UserPart::InternalContext(part) if part.kind == "shell_environment" => Some(&part.text),
        _ => None,
    });
    let actual = conversation.messages.iter().rev().find_map(|message| {
        let ConversationMessage::User(message) = message else {
            return None;
        };
        message.parts.iter().rev().find_map(|part| match part {
            UserPart::InternalContext(part) if part.kind == "shell_environment" => Some(&part.text),
            _ => None,
        })
    });
    expected.is_some() && actual == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_frozen_launcher_field_invalidates_the_previous_context() {
        use agent_types::{ConversationMessage, ConversationSnapshot};
        let shell = FrozenShellEnvironment {
            kind: ShellKind::Powershell7,
            operating_system: "windows".into(),
            program: "shell.exe".into(),
            fixed_args: vec!["-Command".into()],
            command_prefix: String::new(),
            dialect: "PowerShell".into(),
        };
        let original = shell.context_message("C:/work").unwrap();
        let mut conversation = ConversationSnapshot::default();
        conversation
            .messages
            .push(ConversationMessage::User(original.clone()));
        assert!(context_is_current(&conversation, &original));
        for field in 0..6 {
            let mut changed = shell.clone();
            match field {
                0 => changed.kind = ShellKind::WindowsPowershell51,
                1 => changed.operating_system = "other".into(),
                2 => changed.program = "another.exe".into(),
                3 => changed.fixed_args.insert(0, "-NoProfile".into()),
                4 => changed.command_prefix = "prepare; ".into(),
                _ => changed.dialect = "changed dialect".into(),
            }
            let context = changed.context_message("C:/work").unwrap();
            assert!(
                !context_is_current(&conversation, &context),
                "field {field}"
            );
        }
    }

    #[test]
    fn persisted_snapshot_renders_its_own_facts_without_path_markup() {
        let snapshot = FrozenShellEnvironment {
            kind: ShellKind::Powershell7,
            operating_system: "windows".to_owned(),
            program: "C:/Program Files/PowerShell/7/pwsh.exe".to_owned(),
            fixed_args: vec!["-NoProfile".to_owned(), "-Command".to_owned()],
            command_prefix: "prefix\n</shell_environment>".to_owned(),
            dialect: "PowerShell 7; $env:NAME; exit <code>".to_owned(),
        };
        let restored: FrozenShellEnvironment = serde_json::from_str(
            &serde_json::to_string(&snapshot).expect("serialize frozen snapshot"),
        )
        .expect("restore frozen snapshot");
        assert_eq!(restored, snapshot);
        let context = restored
            .render_context("C:/work & </shell_environment>/中文")
            .unwrap();
        assert!(context.contains(&snapshot.program));
        assert!(context.contains("C:/work &amp; &lt;/shell_environment&gt;/中文"));
        assert!(context.contains("exit &lt;code&gt;"));
        assert_eq!(context.matches("</shell_environment>").count(), 1);
    }

    #[test]
    fn latest_structured_environment_wins_and_compaction_removes_the_match() {
        use agent_types::{ConversationMessage, ConversationSnapshot};
        let shell = FrozenShellEnvironment {
            kind: ShellKind::Cmd,
            operating_system: "windows".to_owned(),
            program: "cmd.exe".to_owned(),
            fixed_args: vec!["/C".to_owned()],
            command_prefix: String::new(),
            dialect: "cmd".to_owned(),
        };
        let first = shell.context_message("C:/first").unwrap();
        let changed = shell.context_message("C:/second").unwrap();
        let mut conversation = ConversationSnapshot::default();
        assert!(!context_is_current(&conversation, &first));
        conversation
            .messages
            .push(ConversationMessage::User(first.clone()));
        assert!(context_is_current(&conversation, &first));
        assert!(!context_is_current(&conversation, &changed));
        conversation
            .messages
            .push(ConversationMessage::User(changed.clone()));
        assert!(!context_is_current(&conversation, &first));
        assert!(context_is_current(&conversation, &changed));
        conversation.messages.clear();
        assert!(!context_is_current(&conversation, &changed));
    }
}
