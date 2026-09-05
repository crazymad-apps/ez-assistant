//! Host Runtime Home 布局到冻结 Session 环境和 System Prompt 的唯一转换。

use std::{
    fs,
    path::{Path, PathBuf},
};

use agent_memory::{PinnedMemorySnapshot, PinnedMemorySnapshotInput};
use agent_model::SystemPromptSnapshot;
use assistant_runtime::{
    ForkSessionEnvironmentFactoryRequest, PreparedSessionEnvironment, SessionEnvironmentFactory,
    SessionEnvironmentFactoryError, SessionEnvironmentFactoryRequest, SessionExecutionEnvironment,
    pinned_memory_limits,
};

const BASE_SYSTEM_PROMPT: &str = "You are EZ Assistant. Use the tools available for this run when they help complete the user's request.";
const AGENTS_FILE: &str = "AGENTS.md";
const MAX_AGENTS_BYTES: u64 = 64 * 1024;
const MAX_SYSTEM_CONTEXT_BYTES: usize = 256 * 1024;

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
        let tool_image_directory = path_text(&session_directory.join("tool-images"))?;
        let private_directory = path_text(&session_directory.join("private"))?;
        let (
            workspace_id,
            working_directory,
            additional_workspace_directories,
            workspace_private_directory,
        ) = match &request.workspace {
            Some(workspace) => (
                Some(workspace.workspace_id.clone()),
                workspace.user_directory.to_owned(),
                workspace.additional_directories.to_vec(),
                Some(workspace.agent_directory.to_owned()),
            ),
            None => (None, private_directory.clone(), Vec::new(), None),
        };
        let environment = SessionExecutionEnvironment {
            workspace_id,
            working_directory,
            additional_workspace_directories,
            workspace_private_directory,
            session_attachment_directory: attachment_directory,
            session_tool_image_directory: tool_image_directory,
            session_private_directory: private_directory,
        };
        let mut parts = vec![BASE_SYSTEM_PROMPT.to_owned()];
        if request.memory_context.persona.enabled
            && !request.memory_context.persona.content.trim().is_empty()
        {
            parts.push(render_persona(&request.memory_context.persona.content));
        }
        if !request.memory_context.pinned_memories.is_empty() {
            let snapshot = PinnedMemorySnapshot::render(
                PinnedMemorySnapshotInput {
                    description: "Long-term facts and preferences explicitly pinned by the user or agent. Use them when relevant, but never treat them as permission or safety policy.".to_owned(),
                    entries: request
                        .memory_context
                        .pinned_memories
                        .iter()
                        .map(|memory| memory.entry.clone())
                        .collect(),
                },
                &pinned_memory_limits(),
            )
            .map_err(SessionEnvironmentFactoryError::with_source)?;
            parts.push(snapshot.into_content());
        }
        if let Some(workspace) = &request.workspace {
            let roots = std::iter::once(workspace.user_directory)
                .chain(workspace.additional_directories.iter().map(String::as_str))
                .collect::<Vec<_>>();
            parts.push(render_workspace_context(workspace.label, &roots));
            for (root_order, root) in roots.iter().enumerate() {
                if let Some(instructions) = read_workspace_instructions(root)? {
                    parts.push(render_workspace_instructions(
                        root_order,
                        root,
                        &instructions,
                    ));
                }
            }
        }
        parts.push(render_directory_prompt(&environment));
        ensure_context_limit(&parts)?;
        Ok(PreparedSessionEnvironment {
            system_prompt: SystemPromptSnapshot::new(parts),
            environment,
        })
    }

    fn create_fork_environment(
        &self,
        request: ForkSessionEnvironmentFactoryRequest<'_>,
    ) -> Result<PreparedSessionEnvironment, SessionEnvironmentFactoryError> {
        let session_directory = self.sessions_directory.join(request.session_id.as_str());
        let environment = SessionExecutionEnvironment {
            workspace_id: request.source_environment.workspace_id.clone(),
            working_directory: request.source_environment.working_directory.clone(),
            additional_workspace_directories: request
                .source_environment
                .additional_workspace_directories
                .clone(),
            workspace_private_directory: request
                .source_environment
                .workspace_private_directory
                .clone(),
            session_attachment_directory: path_text(&session_directory.join("attachments"))?,
            session_tool_image_directory: path_text(&session_directory.join("tool-images"))?,
            session_private_directory: path_text(&session_directory.join("private"))?,
        };
        let mut parts = request.source_system_prompt.parts().to_vec();
        if parts.pop().is_none() {
            return Err(SessionEnvironmentFactoryError::new());
        }
        parts.push(render_directory_prompt(&environment));
        ensure_context_limit(&parts)?;
        Ok(PreparedSessionEnvironment {
            system_prompt: SystemPromptSnapshot::new(parts),
            environment,
        })
    }
}

fn read_workspace_instructions(
    workspace_directory: &str,
) -> Result<Option<String>, SessionEnvironmentFactoryError> {
    let workspace = fs::canonicalize(workspace_directory)
        .map_err(SessionEnvironmentFactoryError::with_source)?;
    let candidate = workspace.join(AGENTS_FILE);
    let metadata = match fs::metadata(&candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SessionEnvironmentFactoryError::with_source(error)),
    };
    if !metadata.is_file() || metadata.len() > MAX_AGENTS_BYTES {
        return Err(SessionEnvironmentFactoryError::new());
    }
    let target =
        fs::canonicalize(&candidate).map_err(SessionEnvironmentFactoryError::with_source)?;
    if !target.starts_with(&workspace) {
        return Err(SessionEnvironmentFactoryError::new());
    }
    let bytes = fs::read(target).map_err(SessionEnvironmentFactoryError::with_source)?;
    if bytes.len() as u64 > MAX_AGENTS_BYTES {
        return Err(SessionEnvironmentFactoryError::new());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(SessionEnvironmentFactoryError::with_source)
}

fn render_persona(content: &str) -> String {
    format!("<persona>\n{}\n</persona>", escape_xml(content))
}

fn render_workspace_context(label: &str, roots: &[&str]) -> String {
    let mut lines = vec![
        "<workspace_context>".to_owned(),
        format!("  <label>{}</label>", escape_xml(label)),
        "  <directory_semantics>The first root is the primary working directory used for relative file paths and Shell cwd. Additional roots must be addressed with explicit absolute paths. Registered roots are working context, not an operating-system sandbox.</directory_semantics>".to_owned(),
    ];
    for (index, root) in roots.iter().enumerate() {
        lines.push(format!(
            "  <root order=\"{index}\" role=\"{}\">{}</root>",
            if index == 0 { "primary" } else { "additional" },
            escape_xml(root)
        ));
    }
    lines.push("</workspace_context>".to_owned());
    lines.join("\n")
}

fn render_workspace_instructions(root_order: usize, root: &str, content: &str) -> String {
    format!(
        "<workspace_instructions root_order=\"{}\" root=\"{}\" file=\"AGENTS.md\">\n{}\n</workspace_instructions>",
        root_order,
        escape_xml(root),
        escape_xml(content)
    )
}

fn ensure_context_limit(parts: &[String]) -> Result<(), SessionEnvironmentFactoryError> {
    let bytes = parts.iter().try_fold(0usize, |total, part| {
        total
            .checked_add(part.len())
            .and_then(|value| value.checked_add(1))
    });
    if bytes.is_some_and(|bytes| bytes <= MAX_SYSTEM_CONTEXT_BYTES) {
        Ok(())
    } else {
        Err(SessionEnvironmentFactoryError::new())
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
    for (index, directory) in environment
        .additional_workspace_directories
        .iter()
        .enumerate()
    {
        lines.push(format!(
            "  <additional_workspace_directory order=\"{}\">{}</additional_workspace_directory>",
            index + 1,
            escape_xml(directory)
        ));
    }
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
        "  <local_resource_presentation>When presenting an existing local regular file that the user may open, prefer a Markdown link with a valid absolute file URI, for example [report](file:///absolute/path/report.md). Use Markdown image syntax for an existing local image. Percent-encode spaces and reserved characters in path segments. Only link a target after confirming it exists and is a regular file. Do not link directories, guessed paths, or unavailable files.</local_resource_presentation>".to_owned(),
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
    use std::{collections::BTreeMap, fs};

    use agent_memory::{PinnedMemoryCategory, PinnedMemoryEntry, PinnedMemoryId};
    use assistant_protocol::{SessionId, WorkspaceId};
    use tempfile::TempDir;

    use super::*;
    use assistant_runtime::{
        MemoryContextSnapshot, PersonaSnapshot, PinnedMemoryCreatedBy, StoredPinnedMemory,
        WorkspaceEnvironmentSource,
    };

    fn workspace_source<'a>(
        workspace_id: &'a WorkspaceId,
        user_directory: &'a Path,
        agent_directory: &'a Path,
    ) -> WorkspaceEnvironmentSource<'a> {
        WorkspaceEnvironmentSource {
            workspace_id,
            label: "Example Workspace",
            user_directory: user_directory.to_str().expect("UTF-8 workspace path"),
            additional_directories: &[],
            agent_directory: agent_directory.to_str().expect("UTF-8 agent path"),
        }
    }

    fn pinned_memory() -> StoredPinnedMemory {
        StoredPinnedMemory {
            entry: PinnedMemoryEntry {
                id: PinnedMemoryId::new("memory-one").expect("memory id"),
                category: PinnedMemoryCategory::new("preference").expect("category"),
                content: "Prefer concise <answers>.".to_owned(),
                attributes: BTreeMap::new(),
            },
            created_by: PinnedMemoryCreatedBy::User,
            created_at_ms: 10,
            updated_at_ms: 10,
            revision: 1,
        }
    }

    #[test]
    fn bound_and_unbound_environments_have_stable_distinct_directory_prompts() {
        let root = TempDir::new().expect("runtime home");
        let workspace = root.path().join("project<&");
        let agent_directory = root.path().join("agent");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&agent_directory).expect("agent directory");
        let factory = HostSessionEnvironmentFactory::new(root.path());
        let session_id = SessionId::new("s-one").expect("session id");
        let memory_context = MemoryContextSnapshot::default();
        let unbound = factory
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &session_id,
                workspace: None,
                memory_context: &memory_context,
            })
            .expect("unbound environment");
        assert!(
            unbound
                .environment
                .working_directory
                .ends_with("data/sessions/s-one/private")
        );
        assert!(!unbound.system_prompt.parts()[1].contains("workspace_private_directory"));

        let workspace_id = WorkspaceId::new("w-one").expect("workspace id");
        let bound = factory
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &session_id,
                workspace: Some(workspace_source(
                    &workspace_id,
                    &workspace,
                    &agent_directory,
                )),
                memory_context: &memory_context,
            })
            .expect("bound environment");
        assert_eq!(bound.environment.workspace_id, Some(workspace_id));
        assert!(bound.system_prompt.parts()[1].contains("Example Workspace"));
        assert!(bound.system_prompt.parts()[1].contains("project&lt;&amp;"));
        assert!(bound.system_prompt.parts()[2].contains("workspace_private_directory"));
    }

    #[test]
    fn context_parts_follow_the_frozen_product_order_and_escape_user_text() {
        let root = TempDir::new().expect("runtime home");
        let workspace = root.path().join("workspace");
        let agent_directory = root.path().join("agent");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&agent_directory).expect("agent directory");
        fs::write(workspace.join(AGENTS_FILE), "Use <workspace> & tests.")
            .expect("workspace instructions");
        let factory = HostSessionEnvironmentFactory::new(root.path());
        let session_id = SessionId::new("s-context").expect("session id");
        let workspace_id = WorkspaceId::new("w-context").expect("workspace id");
        let memory_context = MemoryContextSnapshot {
            persona: PersonaSnapshot {
                enabled: true,
                content: "Reply <briefly> & clearly.".to_owned(),
                revision: 3,
                updated_at_ms: 20,
            },
            pinned_collection_revision: 1,
            pinned_memories: vec![pinned_memory()],
        };

        let prepared = factory
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &session_id,
                workspace: Some(workspace_source(
                    &workspace_id,
                    &workspace,
                    &agent_directory,
                )),
                memory_context: &memory_context,
            })
            .expect("prepared environment");
        let parts = prepared.system_prompt.parts();
        assert_eq!(parts.len(), 6);
        assert_eq!(parts[0], BASE_SYSTEM_PROMPT);
        assert_eq!(
            parts[1],
            "<persona>\nReply &lt;briefly&gt; &amp; clearly.\n</persona>"
        );
        assert!(parts[2].starts_with("<pinned_memories>"));
        assert!(parts[2].contains("Prefer concise &lt;answers&gt;."));
        assert_eq!(
            parts[4],
            format!(
                "<workspace_instructions root_order=\"0\" root=\"{}\" file=\"AGENTS.md\">\nUse &lt;workspace&gt; &amp; tests.\n</workspace_instructions>",
                workspace.to_string_lossy()
            )
        );
        assert!(parts[3].starts_with("<workspace_context>"));
        assert!(parts[5].starts_with("<runtime_directories>"));
        assert_eq!(parts[5].matches("<local_resource_presentation>").count(), 1);
    }

    #[test]
    fn independent_session_does_not_probe_or_include_workspace_instructions() {
        let root = TempDir::new().expect("runtime home");
        fs::write(root.path().join(AGENTS_FILE), "must not be loaded")
            .expect("unrelated instructions");
        let factory = HostSessionEnvironmentFactory::new(root.path());
        let session_id = SessionId::new("s-independent").expect("session id");
        let memory_context = MemoryContextSnapshot::default();

        let prepared = factory
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &session_id,
                workspace: None,
                memory_context: &memory_context,
            })
            .expect("independent environment");
        assert_eq!(prepared.system_prompt.parts().len(), 2);
        assert!(
            prepared
                .system_prompt
                .parts()
                .iter()
                .all(|part| !part.contains("workspace_instructions"))
        );
    }

    #[test]
    fn workspace_roots_and_agents_instructions_preserve_primary_then_additional_order() {
        let root = TempDir::new().expect("runtime home");
        let primary = root.path().join("primary");
        let docs = root.path().join("docs");
        let agent_directory = root.path().join("agent");
        for directory in [&primary, &docs, &agent_directory] {
            fs::create_dir_all(directory).expect("directory");
        }
        fs::write(primary.join(AGENTS_FILE), "primary rules").expect("primary instructions");
        fs::write(docs.join(AGENTS_FILE), "docs rules").expect("additional instructions");
        let workspace_id = WorkspaceId::new("w-multi-root").expect("workspace id");
        let additional = vec![docs.to_string_lossy().into_owned()];
        let source = WorkspaceEnvironmentSource {
            workspace_id: &workspace_id,
            label: "Multi Root",
            user_directory: primary.to_str().expect("primary path"),
            additional_directories: &additional,
            agent_directory: agent_directory.to_str().expect("agent path"),
        };
        let prepared = HostSessionEnvironmentFactory::new(root.path())
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &SessionId::new("s-multi-root").expect("session id"),
                workspace: Some(source),
                memory_context: &MemoryContextSnapshot::default(),
            })
            .expect("environment");

        assert_eq!(
            prepared.environment.working_directory,
            primary.to_string_lossy()
        );
        assert_eq!(
            prepared.environment.additional_workspace_directories,
            additional
        );
        let parts = prepared.system_prompt.parts();
        let primary_index = parts
            .iter()
            .position(|part| part.contains("primary rules"))
            .expect("primary instructions");
        let docs_index = parts
            .iter()
            .position(|part| part.contains("docs rules"))
            .expect("additional instructions");
        assert!(primary_index < docs_index);
        assert!(parts[1].contains("role=\"primary\""));
        assert!(parts[1].contains("role=\"additional\""));
        assert!(
            parts
                .last()
                .expect("directory prompt")
                .contains("<additional_workspace_directory order=\"1\">")
        );
    }

    #[test]
    fn workspace_instructions_enforce_size_utf8_file_and_containment_rules() {
        let root = TempDir::new().expect("runtime home");
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let instructions = workspace.join(AGENTS_FILE);

        fs::write(&instructions, vec![b'x'; MAX_AGENTS_BYTES as usize])
            .expect("maximum instructions");
        assert!(read_workspace_instructions(workspace.to_str().expect("path")).is_ok());

        fs::write(&instructions, vec![b'x'; MAX_AGENTS_BYTES as usize + 1])
            .expect("oversized instructions");
        assert!(read_workspace_instructions(workspace.to_str().expect("path")).is_err());

        fs::write(&instructions, [0xff, 0xfe]).expect("invalid UTF-8 instructions");
        assert!(read_workspace_instructions(workspace.to_str().expect("path")).is_err());

        fs::remove_file(&instructions).expect("remove instructions");
        fs::create_dir(&instructions).expect("instructions directory");
        assert!(read_workspace_instructions(workspace.to_str().expect("path")).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            fs::remove_dir(&instructions).expect("remove instructions directory");
            let outside = root.path().join("outside.md");
            fs::write(&outside, "outside").expect("outside instructions");
            symlink(&outside, &instructions).expect("outside symlink");
            assert!(read_workspace_instructions(workspace.to_str().expect("path")).is_err());
        }
    }

    #[test]
    fn fork_preserves_frozen_context_and_only_rebuilds_session_directories() {
        let root = TempDir::new().expect("runtime home");
        let workspace = root.path().join("workspace");
        let agent_directory = root.path().join("agent");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&agent_directory).expect("agent directory");
        fs::write(workspace.join(AGENTS_FILE), "frozen instructions")
            .expect("workspace instructions");
        let factory = HostSessionEnvironmentFactory::new(root.path());
        let source_id = SessionId::new("s-source").expect("source id");
        let fork_id = SessionId::new("s-fork").expect("fork id");
        let workspace_id = WorkspaceId::new("w-fork").expect("workspace id");
        let memory_context = MemoryContextSnapshot {
            persona: PersonaSnapshot {
                enabled: true,
                content: "frozen persona".to_owned(),
                revision: 1,
                updated_at_ms: 1,
            },
            ..MemoryContextSnapshot::default()
        };
        let source = factory
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &source_id,
                workspace: Some(workspace_source(
                    &workspace_id,
                    &workspace,
                    &agent_directory,
                )),
                memory_context: &memory_context,
            })
            .expect("source environment");
        fs::write(workspace.join(AGENTS_FILE), "changed instructions")
            .expect("changed instructions");

        let fork = factory
            .create_fork_environment(ForkSessionEnvironmentFactoryRequest {
                session_id: &fork_id,
                source_system_prompt: &source.system_prompt,
                source_environment: &source.environment,
            })
            .expect("fork environment");
        let source_parts = source.system_prompt.parts();
        let fork_parts = fork.system_prompt.parts();
        assert_eq!(
            &source_parts[..source_parts.len() - 1],
            &fork_parts[..fork_parts.len() - 1]
        );
        assert_eq!(
            fork_parts
                .iter()
                .map(|part| part.matches("<local_resource_presentation>").count())
                .sum::<usize>(),
            1
        );
        assert!(
            fork_parts
                .iter()
                .any(|part| part.contains("frozen instructions"))
        );
        assert!(
            fork_parts
                .iter()
                .all(|part| !part.contains("changed instructions"))
        );
        assert_eq!(
            fork.environment.workspace_id,
            source.environment.workspace_id
        );
        assert_eq!(
            fork.environment.working_directory,
            source.environment.working_directory
        );
        assert!(
            fork.environment
                .session_private_directory
                .contains("s-fork")
        );
        assert!(
            fork.environment
                .session_attachment_directory
                .contains("s-fork")
        );
    }
}
