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
    personal: bool,
    paths: std::sync::Arc<crate::user_paths::UserPaths>,
}

impl HostSessionEnvironmentFactory {
    #[cfg(test)]
    pub(super) fn new(runtime_home: &Path) -> Self {
        Self::with_paths(
            runtime_home,
            true,
            std::sync::Arc::new(crate::user_paths::UserPaths::new(runtime_home, Vec::new())),
        )
    }
    pub(super) fn with_paths(
        runtime_home: &Path,
        personal: bool,
        paths: std::sync::Arc<crate::user_paths::UserPaths>,
    ) -> Self {
        Self {
            paths,
            personal,
            // 与 Store 使用同样的原生路径组件，避免 Windows Fork 校验中的分隔符差异。
            sessions_directory: runtime_home.join("data").join("sessions"),
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
                workspace
                    .user_directory
                    .unwrap_or(workspace.agent_directory)
                    .to_owned(),
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
            let roots = std::iter::once(
                workspace
                    .user_directory
                    .unwrap_or(workspace.agent_directory),
            )
            .chain(workspace.additional_directories.iter().map(String::as_str))
            .collect::<Vec<_>>();
            parts.push(render_workspace_context(
                workspace.label,
                &roots,
                workspace.user_directory.is_none(),
            ));
            for (root_order, root) in roots.iter().enumerate() {
                self.paths
                    .resolve(Path::new(root), false)
                    .map_err(SessionEnvironmentFactoryError::with_source)?;
                self.paths
                    .resolve(&Path::new(root).join(AGENTS_FILE), false)
                    .map_err(SessionEnvironmentFactoryError::with_source)?;
                if let Some(instructions) = read_workspace_instructions(root)? {
                    parts.push(render_workspace_instructions(
                        root_order,
                        root,
                        &instructions,
                    ));
                }
            }
        }
        parts.push(render_directory_prompt(
            &environment,
            &self.paths.user_root,
            self.personal,
        )?);
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
        let private_directory = path_text(&session_directory.join("private"))?;
        let environment = SessionExecutionEnvironment {
            workspace_id: request.source_environment.workspace_id.clone(),
            // 无工作区会话的 cwd 属于自身 private；Fork 不能继续指向源会话的私有目录。
            working_directory: if request.source_environment.workspace_id.is_some() {
                request.source_environment.working_directory.clone()
            } else {
                private_directory.clone()
            },
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
            session_private_directory: private_directory,
        };
        let mut parts = request.source_system_prompt.parts().to_vec();
        // Catalog 和自定义 Agent 指令插在目录部分之前；只替换完整的 Host 最后部分，
        // 不扫描用户正文，也不以删除未知指令的方式修复不符合约定的快照。
        let last = parts
            .last_mut()
            .ok_or_else(SessionEnvironmentFactoryError::new)?;
        let body = last
            .strip_prefix("<runtime_directories>")
            .and_then(|body| body.strip_suffix("</runtime_directories>"))
            .ok_or_else(SessionEnvironmentFactoryError::new)?;
        if body.contains("<runtime_directories") || body.contains("</runtime_directories") {
            return Err(SessionEnvironmentFactoryError::new());
        }
        *last = render_directory_prompt(&environment, &self.paths.user_root, self.personal)?;
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

fn render_workspace_context(label: &str, roots: &[&str], private_primary: bool) -> String {
    let mut lines = vec![
        "<workspace_context>".to_owned(),
        format!("  <label>{}</label>", escape_xml(label)),
        if private_primary {
            "  <directory_semantics>The first root is this Workspace's managed private working directory, used for relative file paths and Shell cwd. It is shared by Sessions in this Workspace. Registered roots are working context, not an operating-system sandbox.</directory_semantics>".to_owned()
        } else {
            "  <directory_semantics>The first root is the primary working directory used for relative file paths and Shell cwd. Additional roots must be addressed with explicit absolute paths. Registered roots are working context, not an operating-system sandbox.</directory_semantics>".to_owned()
        },
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

fn render_directory_prompt(
    environment: &SessionExecutionEnvironment,
    user_root: &Path,
    personal: bool,
) -> Result<String, SessionEnvironmentFactoryError> {
    // 直接使用本用户服务装配时固定的根目录；不从工作区、OS Home 或源会话提示词猜测。
    let mut lines = vec![
        "<runtime_directories>".to_owned(),
        format!(
            "  <runtime_mode>{}</runtime_mode>",
            if personal { "personal" } else { "enterprise" }
        ),
        "  <runtime_execution_semantics>This session's directories and tool execution belong to the connected EZ Assistant Runtime Host and current user domain. The client may run on another device; its local paths are not Host paths. runtime_mode describes configuration ownership, not tool permission. Enterprise users may also manage personal settings; check each resource's source and the available tools.</runtime_execution_semantics>".to_owned(),
        format!(
            "  <user_data_directory>{}</user_data_directory>",
            escape_xml(&path_text(user_root)?)
        ),
        format!(
            "  <user_skills_directory>{}</user_skills_directory>",
            escape_xml(&path_text(&user_root.join("skills"))?)
        ),
        "  <user_directory_semantics>user_data_directory is the current EZ Assistant user's private data root, not the operating-system home or a workspace. When creating or installing skills, prefer user_skills_directory, with each skill at &lt;skill-name&gt;/SKILL.md. If needed, create the skill directory using the normal file tools. These paths describe locations and do not grant permission; follow the existing tool permission rules.</user_directory_semantics>".to_owned(),
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
    Ok(lines.join("\n"))
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

    use crate::resources::{HostResources, model::HostModelServiceFactory};

    use super::*;
    use assistant_runtime::{
        MemoryContextSnapshot, PersonaSnapshot, PinnedMemoryCreatedBy, StoredPinnedMemory,
        WorkspaceEnvironmentSource,
    };

    fn runtime_home() -> TempDir {
        let test_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        fs::create_dir_all(&test_root).expect("test directory");
        tempfile::Builder::new()
            .prefix("session-environment-")
            .tempdir_in(fs::canonicalize(test_root).expect("test root"))
            .expect("isolated runtime home")
    }

    #[test]
    fn personal_and_enterprise_prompts_use_the_actual_domain_not_its_path() {
        let root = runtime_home();
        // 刻意反转目录名称，证明模式由正式用户资源装配传递，而非按路径推导。
        for (personal, user, mode) in [
            (true, "_personal", "personal"),
            (false, "center-user_1", "enterprise"),
            (false, "_personal", "enterprise"),
            (true, "center-user_1", "personal"),
        ] {
            let user_root = root.path().join("host&data/users").join(user);
            let resources = HostResources::for_user(
                &user_root,
                personal,
                std::sync::Arc::new(HostModelServiceFactory::new(&user_root)),
                std::sync::Arc::new(crate::user_paths::UserPaths::new(&user_root, Vec::new())),
                None,
            )
            .expect("actual user resources");
            let factory = resources.session_environment_factory;
            let source = factory
                .create_environment(SessionEnvironmentFactoryRequest {
                    session_id: &SessionId::new("source").expect("session id"),
                    workspace: None,
                    memory_context: &MemoryContextSnapshot::default(),
                })
                .expect("user environment");
            let prompt = source.system_prompt.parts().last().expect("directories");
            let user_tag = format!(
                "<user_data_directory>{}</user_data_directory>",
                escape_xml(user_root.to_str().expect("user path"))
            );
            let skills_tag = format!(
                "<user_skills_directory>{}</user_skills_directory>",
                escape_xml(user_root.join("skills").to_str().expect("skills path"))
            );
            assert!(prompt.contains(&user_tag));
            assert!(prompt.contains(&skills_tag));
            assert!(prompt.contains(&format!("<runtime_mode>{mode}</runtime_mode>")));
            assert_eq!(prompt.matches("<runtime_mode>").count(), 1);
            assert!(prompt.contains("connected EZ Assistant Runtime Host"));
            assert!(prompt.contains("not tool permission"));
            assert!(prompt.contains("host&amp;data"));
            assert_ne!(
                source.environment.working_directory,
                user_root.to_string_lossy()
            );

            // 旧会话即使尚未记录用户目录，分叉仍从当前用户装配重建目录，不依赖父提示词猜路径。
            let mut old_parts = source.system_prompt.parts().to_vec();
            *old_parts.last_mut().expect("directory part") =
                "<runtime_directories>legacy session directories</runtime_directories>".to_owned();
            let fork = factory
                .create_fork_environment(ForkSessionEnvironmentFactoryRequest {
                    session_id: &SessionId::new("fork").expect("fork id"),
                    source_system_prompt: &SystemPromptSnapshot::new(old_parts),
                    source_environment: &source.environment,
                })
                .expect("fork environment");
            let prompt = fork.system_prompt.parts().last().expect("fork directories");
            assert_eq!(prompt.matches(&user_tag).count(), 1);
            assert_eq!(prompt.matches(&skills_tag).count(), 1);
            assert!(prompt.contains(&format!("<runtime_mode>{mode}</runtime_mode>")));
            assert!(prompt.contains("fork"));
            assert!(!prompt.contains("legacy session directories"));
        }
    }

    fn workspace_source<'a>(
        workspace_id: &'a WorkspaceId,
        user_directory: &'a Path,
        agent_directory: &'a Path,
    ) -> WorkspaceEnvironmentSource<'a> {
        WorkspaceEnvironmentSource {
            workspace_id,
            label: "Example Workspace",
            user_directory: Some(user_directory.to_str().expect("UTF-8 workspace path")),
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
        let root = runtime_home();
        let workspace = root.path().join("project&");
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
        assert_eq!(
            unbound.environment.working_directory,
            root.path()
                .join("data")
                .join("sessions")
                .join("s-one")
                .join("private")
                .to_str()
                .unwrap()
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
        assert!(bound.system_prompt.parts()[1].contains("project&amp;"));
        assert!(bound.system_prompt.parts()[2].contains("workspace_private_directory"));
    }

    #[test]
    fn private_workspace_uses_its_agent_directory_for_cwd_and_agents_instructions() {
        let root = runtime_home();
        let agent_directory = root.path().join("data/workspaces/w-private/agent");
        fs::create_dir_all(&agent_directory).expect("private workspace directory");
        fs::write(
            agent_directory.join(AGENTS_FILE),
            "Follow private workspace rules.",
        )
        .expect("private workspace instructions");
        let workspace_id = WorkspaceId::new("w-private").expect("workspace id");
        let source = WorkspaceEnvironmentSource {
            workspace_id: &workspace_id,
            label: "Private Workspace",
            user_directory: None,
            additional_directories: &[],
            agent_directory: agent_directory.to_str().expect("UTF-8 agent path"),
        };
        let prepared = HostSessionEnvironmentFactory::new(root.path())
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &SessionId::new("s-private").expect("session id"),
                workspace: Some(source),
                memory_context: &MemoryContextSnapshot::default(),
            })
            .expect("private workspace environment");
        assert_eq!(
            prepared.environment.working_directory,
            agent_directory.to_string_lossy()
        );
        let parts = prepared.system_prompt.parts();
        assert!(
            parts
                .iter()
                .any(|part| part.contains("managed private working directory"))
        );
        assert!(
            parts
                .iter()
                .any(|part| part.contains("Follow private workspace rules."))
        );
    }

    #[test]
    fn context_parts_follow_the_frozen_product_order_and_escape_user_text() {
        let root = runtime_home();
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
        let root = runtime_home();
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
        let root = runtime_home();
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
            user_directory: Some(primary.to_str().expect("primary path")),
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
        let root = runtime_home();
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
        let root = runtime_home();
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

    #[test]
    fn fork_rebuilds_target_identity_and_preserves_literal_user_tags() {
        let root = runtime_home();
        let source = HostSessionEnvironmentFactory::new(root.path())
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &SessionId::new("source").expect("source id"),
                workspace: None,
                memory_context: &MemoryContextSnapshot::default(),
            })
            .expect("source environment");
        let mut parts = source.system_prompt.into_parts();
        // 自定义 Agent 正文可以含同名标签；只拥有最后的完整环境部分。
        let literal =
            "<runtime_directories><runtime_mode>user example</runtime_mode></runtime_directories>";
        parts.insert(1, literal.to_owned());
        let frozen = SystemPromptSnapshot::new(parts);
        let original = frozen.clone();
        let target_home = root.path().join("other-domain");
        let target = HostSessionEnvironmentFactory::with_paths(
            &target_home,
            false,
            std::sync::Arc::new(crate::user_paths::UserPaths::new(&target_home, Vec::new())),
        );
        let fork = target
            .create_fork_environment(ForkSessionEnvironmentFactoryRequest {
                session_id: &SessionId::new("fork").expect("fork id"),
                source_system_prompt: &frozen,
                source_environment: &source.environment,
            })
            .expect("fork environment");
        assert_eq!(frozen, original, "source is never rewritten");
        assert_eq!(fork.system_prompt.parts()[1], literal);
        assert_eq!(&fork.system_prompt.parts()[..2], &frozen.parts()[..2]);
        let directories = fork.system_prompt.parts().last().expect("directories");
        assert!(directories.contains("<runtime_mode>enterprise</runtime_mode>"));
        assert!(directories.contains(target_home.to_str().expect("target path")));
        assert!(!directories.contains(&source.environment.session_private_directory));
        assert_eq!(fork.environment.workspace_id, None);
        assert_eq!(
            fork.environment.working_directory,
            fork.environment.session_private_directory
        );
        assert_ne!(
            fork.environment.session_private_directory,
            source.environment.session_private_directory
        );
        assert_ne!(
            fork.environment.session_tool_image_directory,
            source.environment.session_tool_image_directory
        );
    }

    #[test]
    fn fork_rejects_missing_malformed_or_nonfinal_directory_parts() {
        let root = runtime_home();
        let factory = HostSessionEnvironmentFactory::new(root.path());
        let source = factory
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &SessionId::new("source").expect("source id"),
                workspace: None,
                memory_context: &MemoryContextSnapshot::default(),
            })
            .expect("source environment");
        let directory = source.system_prompt.parts().last().expect("directories");
        let snapshots = [
            Vec::new(),
            vec![BASE_SYSTEM_PROMPT.to_owned()],
            vec![directory.clone(), "must not be discarded".to_owned()],
            vec![format!("prefix{directory}")],
            vec![format!("{directory}suffix")],
            vec!["<runtime_directories>missing closing tag".to_owned()],
            vec![format!("{directory}{directory}")],
            vec![format!(
                "<runtime_directories>{directory}</runtime_directories>"
            )],
        ];
        for parts in snapshots {
            let frozen = SystemPromptSnapshot::new(parts);
            let original = frozen.clone();
            assert!(
                factory
                    .create_fork_environment(ForkSessionEnvironmentFactoryRequest {
                        session_id: &SessionId::new("fork").expect("fork id"),
                        source_system_prompt: &frozen,
                        source_environment: &source.environment,
                    })
                    .is_err()
            );
            assert_eq!(frozen, original);
        }
    }

    #[test]
    fn create_and_fork_enforce_the_complete_prompt_size_limit() {
        let root = runtime_home();
        let factory = HostSessionEnvironmentFactory::new(root.path());
        let id = SessionId::new("source").expect("source id");
        let oversized = MemoryContextSnapshot {
            persona: PersonaSnapshot {
                enabled: true,
                content: "x".repeat(MAX_SYSTEM_CONTEXT_BYTES),
                revision: 1,
                updated_at_ms: 1,
            },
            ..MemoryContextSnapshot::default()
        };
        assert!(
            factory
                .create_environment(SessionEnvironmentFactoryRequest {
                    session_id: &id,
                    workspace: None,
                    memory_context: &oversized,
                })
                .is_err()
        );
        let source = factory
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &id,
                workspace: None,
                memory_context: &MemoryContextSnapshot::default(),
            })
            .expect("source environment");
        let frozen = SystemPromptSnapshot::new(vec![
            "x".repeat(MAX_SYSTEM_CONTEXT_BYTES),
            source
                .system_prompt
                .parts()
                .last()
                .expect("directories")
                .clone(),
        ]);
        assert!(
            factory
                .create_fork_environment(ForkSessionEnvironmentFactoryRequest {
                    session_id: &SessionId::new("fork").expect("fork id"),
                    source_system_prompt: &frozen,
                    source_environment: &source.environment,
                })
                .is_err()
        );
    }
}
