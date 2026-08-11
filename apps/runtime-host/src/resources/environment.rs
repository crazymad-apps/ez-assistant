//! Host Runtime Home 布局到冻结 Session 环境和 System Prompt 的唯一转换。

use std::path::{Path, PathBuf};

use agent_model::SystemPromptSnapshot;
use assistant_runtime::{
    PreparedSessionEnvironment, SessionEnvironmentFactory, SessionEnvironmentFactoryError,
    SessionEnvironmentFactoryRequest, SessionExecutionEnvironment,
};

const BASE_SYSTEM_PROMPT: &str = "You are EZ Assistant. Use the tools available for this run when they help complete the user's request.";

pub(super) struct HostSessionEnvironmentFactory {
    sessions_directory: PathBuf,
}

impl HostSessionEnvironmentFactory {
    pub(super) fn new(runtime_home: &Path) -> Self {
        Self {
            sessions_directory: runtime_home.join("data/sessions"),
        }
    }
}

impl SessionEnvironmentFactory for HostSessionEnvironmentFactory {
    fn create_environment(
        &self,
        request: SessionEnvironmentFactoryRequest<'_>,
    ) -> Result<PreparedSessionEnvironment, SessionEnvironmentFactoryError> {
        let session_directory = self.sessions_directory.join(request.session_id.as_str());
        let attachment_directory = path_text(&session_directory.join("attachments"))?;
        let private_directory = path_text(&session_directory.join("private"))?;
        let (workspace_id, working_directory, workspace_private_directory) = match request.workspace
        {
            Some(workspace) => (
                Some(workspace.workspace_id.clone()),
                workspace.user_directory.to_owned(),
                Some(workspace.agent_directory.to_owned()),
            ),
            None => (None, private_directory.clone(), None),
        };
        let environment = SessionExecutionEnvironment {
            workspace_id,
            working_directory,
            workspace_private_directory,
            session_attachment_directory: attachment_directory,
            session_private_directory: private_directory,
        };
        let directory_prompt = render_directory_prompt(&environment);
        Ok(PreparedSessionEnvironment {
            system_prompt: SystemPromptSnapshot::new(vec![
                BASE_SYSTEM_PROMPT.to_owned(),
                directory_prompt,
            ]),
            environment,
        })
    }
}

fn path_text(path: &Path) -> Result<String, SessionEnvironmentFactoryError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(SessionEnvironmentFactoryError::new)
}

fn render_directory_prompt(environment: &SessionExecutionEnvironment) -> String {
    let mut lines = vec![
        "<runtime_directories>".to_owned(),
        format!(
            "  <working_directory>{}</working_directory>",
            escape_xml(&environment.working_directory)
        ),
    ];
    if let Some(directory) = environment.workspace_private_directory.as_deref() {
        lines.push(format!(
            "  <workspace_private_directory>{}</workspace_private_directory>",
            escape_xml(directory)
        ));
    }
    lines.extend([
        format!(
            "  <session_attachment_directory>{}</session_attachment_directory>",
            escape_xml(&environment.session_attachment_directory)
        ),
        format!(
            "  <session_private_directory>{}</session_private_directory>",
            escape_xml(&environment.session_private_directory)
        ),
        "</runtime_directories>".to_owned(),
    ]);
    lines.join("\n")
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use assistant_protocol::{SessionId, WorkspaceId};

    use super::*;
    use assistant_runtime::WorkspaceEnvironmentSource;

    #[test]
    fn bound_and_unbound_environments_have_stable_distinct_directory_prompts() {
        let factory = HostSessionEnvironmentFactory::new(Path::new("/runtime<&"));
        let session_id = SessionId::new("s-one").expect("session id");
        let unbound = factory
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &session_id,
                workspace: None,
            })
            .expect("unbound environment");
        assert_eq!(
            unbound.environment.working_directory,
            "/runtime<&/data/sessions/s-one/private"
        );
        assert!(!unbound.system_prompt.parts()[1].contains("workspace_private_directory"));
        assert!(unbound.system_prompt.parts()[1].contains("/runtime&lt;&amp;"));

        let workspace_id = WorkspaceId::new("w-one").expect("workspace id");
        let bound = factory
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &session_id,
                workspace: Some(WorkspaceEnvironmentSource {
                    workspace_id: &workspace_id,
                    user_directory: "/project<&",
                    agent_directory: "/runtime/agent",
                }),
            })
            .expect("bound environment");
        assert_eq!(bound.environment.workspace_id, Some(workspace_id));
        assert!(bound.system_prompt.parts()[1].contains("/project&lt;&amp;"));
        assert!(bound.system_prompt.parts()[1].contains("workspace_private_directory"));
    }
}
