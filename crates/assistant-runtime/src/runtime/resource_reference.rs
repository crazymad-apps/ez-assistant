//! 已完成恢复校验之后的请求副本适配；不修改 Journal、C02 摘要或自由文本。

use crate::{RuntimeResult, RuntimeStore, SessionExecutionEnvironment, StoredAttachment};
use agent_types::{ConversationMessage, ConversationSnapshot, UserMessage, UserPart};
use std::collections::BTreeMap;

#[derive(Clone, Default)]
pub(crate) struct ResourceReferenceProjection {
    aliases: BTreeMap<String, String>,
    directories: Option<UserMessage>,
}

impl ResourceReferenceProjection {
    pub(crate) fn new<'a>(
        store: &dyn RuntimeStore,
        attachments: impl Iterator<Item = &'a StoredAttachment>,
        environment: &SessionExecutionEnvironment,
        prompt: &agent_model::SystemPromptSnapshot,
    ) -> RuntimeResult<Self> {
        let aliases = attachments
            .filter_map(|a| {
                store
                    .historical_attachment_path(a)
                    .map(|old| (old, a.agent_readable_path.clone()))
            })
            .collect();
        let directories = if prompt.parts().iter().any(|p| {
            p.contains("<runtime_directories>")
                && !p.contains(&environment.session_private_directory)
        }) {
            use crate::internal_boundary::{
                InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
            };
            let paths = serde_json::json!({
                "working_directory": environment.working_directory,
                "additional_workspace_directories": environment.additional_workspace_directories,
                "workspace_private_directory": environment.workspace_private_directory,
                "session_private_directory": environment.session_private_directory,
                "session_attachment_directory": environment.session_attachment_directory,
            });
            Some(InternalBoundaryCoordinator::hidden_message(InternalBoundaryRequest {
                source: InternalBoundarySource::RuntimeDirectories,
                text: format!("Current runtime directories supersede the historical directory locations in the system prompt: {paths}"),
            })?.0)
        } else {
            None
        };
        Ok(Self {
            aliases,
            directories,
        })
    }

    pub(crate) fn apply(&self, mut conversation: ConversationSnapshot) -> ConversationSnapshot {
        for message in &mut conversation.messages {
            if let ConversationMessage::User(user) = message {
                for part in &mut user.parts {
                    if let UserPart::FileReferences(refs) = part {
                        for file in &mut refs.files {
                            if let Some(current) = self.aliases.get(&file.readable_path) {
                                file.readable_path = current.clone();
                            }
                        }
                    }
                }
            }
        }
        if let Some(message) = &self.directories {
            conversation
                .messages
                .push(ConversationMessage::User(message.clone()));
        }
        conversation
    }

    /// Child agents already receive fresh delegation directories in their own prompt.
    pub(crate) fn for_child(&self) -> Self {
        Self {
            aliases: self.aliases.clone(),
            directories: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_types::{FileReference, FileReferencesPart, MessageId, PartId, TextPart};

    #[test]
    fn request_projection_preserves_source_history_and_unknown_references() {
        let old = "/home/data/sessions/s/attachments/a/file.txt";
        let current = "/home/users/_personal/data/sessions/s/attachments/a/file.txt";
        let source = ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
            id: MessageId::new("message").unwrap(),
            origin: Default::default(),
            transcript_visibility: Default::default(),
            parts: vec![
                UserPart::Text(TextPart {
                    id: PartId::new("text").unwrap(),
                    text: old.into(),
                }),
                UserPart::FileReferences(FileReferencesPart {
                    id: PartId::new("files").unwrap(),
                    files: vec![
                        FileReference {
                            original_name: "file.txt".into(),
                            readable_path: old.into(),
                        },
                        FileReference {
                            original_name: "other.txt".into(),
                            readable_path: "/home/data/sessions/other/private/other.txt".into(),
                        },
                    ],
                }),
            ],
        })]);
        let bytes = serde_json::to_vec(&source).unwrap();
        let projection = ResourceReferenceProjection {
            aliases: BTreeMap::from([(old.into(), current.into())]),
            directories: None,
        };
        let request = projection.apply(source.clone());
        assert_eq!(serde_json::to_vec(&source).unwrap(), bytes);
        let ConversationMessage::User(original) = &source.messages[0] else {
            panic!("user")
        };
        let ConversationMessage::User(actual) = &request.messages[0] else {
            panic!("user")
        };
        assert_eq!(actual.parts[0], original.parts[0]);
        let UserPart::FileReferences(files) = &actual.parts[1] else {
            panic!("files")
        };
        assert_eq!(files.files[0].readable_path, current);
        assert_eq!(
            files.files[1].readable_path,
            "/home/data/sessions/other/private/other.txt"
        );
        assert_eq!(projection.for_child().apply(source), request);
    }
}
