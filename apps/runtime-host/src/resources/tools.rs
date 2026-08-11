//! 基于 Session 冻结目录为每个 Run 组装工具快照和授权闸。

use std::{
    ffi::OsString,
    num::{NonZeroU32, NonZeroU64},
    path::Path,
    sync::Arc,
    time::Duration,
};

use agent_core::{AuthorizationFuture, ToolAuthorization, ToolAuthorizer};
use agent_tools::{
    AbsolutePath, FileAuthorizationFacts, FileOperation, FsDeleteTool, FsEditTool, FsFindTool,
    FsListTool, FsReadTool, FsSearchTool, FsWriteTool, GeneralAuthorizationFacts,
    ReadFileToolConfig, ResolvedToolBatch, ResolvedToolInvocation, SearchFilesToolConfig,
    SessionPathResolver, ShellAuthorizationFacts, ShellExecTool, ShellExecToolConfig, Tool,
    ToolContext, ToolError, ToolExecuteFuture, ToolRegistry, ToolResolution,
};
use agent_tools_local::{
    EnvironmentPolicy, LocalFileSystem, LocalFileSystemConfig, LocalShell, LocalShellConfig,
};
use agent_types::ToolName;
use assistant_runtime::{
    RunToolBundle, RunToolFactory, RunToolFactoryError, RunToolFactoryErrorKind,
    SessionExecutionEnvironment,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const ECHO_TOOL_NAME: &str = "echo_text";
const MAX_TEXT_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SEARCH_OUTPUT_BYTES: u64 = 1024 * 1024;
const MAX_SEARCH_RECORD_BYTES: u64 = 64 * 1024;
const MAX_SEARCH_STDERR_BYTES: u64 = 64 * 1024;
const MAX_SHELL_OUTPUT_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LocalToolMode {
    Safe,
    UnsafeUnrestricted,
}

pub(super) struct HostRunToolFactory {
    mode: LocalToolMode,
    unrestricted: Option<UnrestrictedResources>,
    sessions_root: AbsolutePath,
}

struct UnrestrictedResources {
    filesystem: Arc<LocalFileSystem>,
    shell: Arc<LocalShell>,
    read_config: ReadFileToolConfig,
    search_config: SearchFilesToolConfig,
    shell_config: ShellExecToolConfig,
}

impl HostRunToolFactory {
    pub(super) fn new(mode: LocalToolMode, runtime_home: &Path) -> Result<Self, ToolResourceError> {
        let unrestricted = match mode {
            LocalToolMode::Safe => None,
            LocalToolMode::UnsafeUnrestricted => Some(UnrestrictedResources::new()?),
        };
        let sessions_root = AbsolutePath::new(runtime_home.join("data/sessions"))
            .map_err(ToolResourceError::path)?;
        Ok(Self {
            mode,
            unrestricted,
            sessions_root,
        })
    }
}

impl RunToolFactory for HostRunToolFactory {
    fn compile(
        &self,
        environment: &SessionExecutionEnvironment,
    ) -> Result<RunToolBundle, RunToolFactoryError> {
        let resolver = checked_resolver(environment)?;
        match self.mode {
            LocalToolMode::Safe => safe_bundle(),
            LocalToolMode::UnsafeUnrestricted => self
                .unrestricted
                .as_ref()
                .expect("unrestricted mode has resources")
                .compile(environment, resolver, self.sessions_root.clone()),
        }
    }
}

impl UnrestrictedResources {
    fn new() -> Result<Self, ToolResourceError> {
        let read_config = ReadFileToolConfig::new(nonzero32(1), nonzero32(200), nonzero32(2_000))
            .map_err(ToolResourceError::configuration)?;
        let search_config = SearchFilesToolConfig::new(
            nonzero32(100),
            nonzero32(1_000),
            nonzero64(MAX_SEARCH_OUTPUT_BYTES),
            nonzero64(MAX_SEARCH_RECORD_BYTES),
        )
        .map_err(ToolResourceError::configuration)?;
        let shell_config = ShellExecToolConfig::new(
            Duration::from_secs(30),
            Duration::from_secs(120),
            nonzero64(MAX_SHELL_OUTPUT_BYTES),
        )
        .map_err(ToolResourceError::configuration)?;
        Ok(Self {
            filesystem: Arc::new(LocalFileSystem::new(LocalFileSystemConfig {
                max_text_file_bytes: nonzero64(MAX_TEXT_FILE_BYTES),
                ripgrep_program: OsString::from("rg"),
                max_search_stderr_bytes: nonzero64(MAX_SEARCH_STDERR_BYTES),
            })),
            shell: Arc::new(LocalShell::new(LocalShellConfig::new(
                EnvironmentPolicy::default(),
            ))),
            read_config,
            search_config,
            shell_config,
        })
    }

    fn compile(
        &self,
        environment: &SessionExecutionEnvironment,
        resolver: SessionPathResolver,
        sessions_root: AbsolutePath,
    ) -> Result<RunToolBundle, RunToolFactoryError> {
        let mut registry = ToolRegistry::new();
        register(&mut registry, EchoTextTool)?;
        register(
            &mut registry,
            FsReadTool::new(self.filesystem.clone(), resolver.clone(), self.read_config),
        )?;
        register(
            &mut registry,
            FsListTool::new(self.filesystem.clone(), resolver.clone()),
        )?;
        register(
            &mut registry,
            FsFindTool::new(
                self.filesystem.clone(),
                resolver.clone(),
                self.search_config,
            ),
        )?;
        register(
            &mut registry,
            FsSearchTool::new(
                self.filesystem.clone(),
                resolver.clone(),
                self.search_config,
            ),
        )?;
        register(
            &mut registry,
            FsWriteTool::new(self.filesystem.clone(), resolver.clone()),
        )?;
        register(
            &mut registry,
            FsEditTool::new(self.filesystem.clone(), resolver.clone()),
        )?;
        register(
            &mut registry,
            FsDeleteTool::new(self.filesystem.clone(), resolver.clone()),
        )?;
        register(
            &mut registry,
            ShellExecTool::new(self.shell.clone(), resolver, self.shell_config),
        )?;
        // 校验当前 Session 冻结附件目录的类型边界。Authorizer 持有
        // Runtime Home 下的 sessions root，因此同样保护其他 Session 附件。
        AbsolutePath::new(&environment.session_attachment_directory).map_err(|source| {
            RunToolFactoryError::with_source(RunToolFactoryErrorKind::InvalidConfiguration, source)
        })?;
        Ok(RunToolBundle::new(
            registry.snapshot(),
            Arc::new(UnrestrictedLocalAuthorizer { sessions_root }),
        ))
    }
}

fn checked_resolver(
    environment: &SessionExecutionEnvironment,
) -> Result<SessionPathResolver, RunToolFactoryError> {
    let workdir = AbsolutePath::new(&environment.working_directory).map_err(|source| {
        RunToolFactoryError::with_source(RunToolFactoryErrorKind::InvalidConfiguration, source)
    })?;
    let metadata = std::fs::metadata(workdir.as_path()).map_err(|source| {
        RunToolFactoryError::with_source(
            RunToolFactoryErrorKind::WorkingDirectoryUnavailable,
            source,
        )
    })?;
    if !metadata.is_dir() {
        return Err(RunToolFactoryError::new(
            RunToolFactoryErrorKind::WorkingDirectoryUnavailable,
        ));
    }
    Ok(SessionPathResolver::new(workdir))
}

fn safe_bundle() -> Result<RunToolBundle, RunToolFactoryError> {
    let mut registry = ToolRegistry::new();
    register(&mut registry, EchoTextTool)?;
    Ok(RunToolBundle::new(
        registry.snapshot(),
        Arc::new(EchoOnlyAuthorizer),
    ))
}

fn register<T: Tool>(registry: &mut ToolRegistry, tool: T) -> Result<(), RunToolFactoryError> {
    registry.register(tool).map_err(|source| {
        RunToolFactoryError::with_source(RunToolFactoryErrorKind::InvalidConfiguration, source)
    })
}

struct EchoOnlyAuthorizer;

impl ToolAuthorizer for EchoOnlyAuthorizer {
    fn authorize<'a>(
        &'a self,
        invocation: &'a ResolvedToolInvocation,
        _batch: &'a ResolvedToolBatch,
    ) -> AuthorizationFuture<'a> {
        let decision = if invocation.tool_name().as_str() == ECHO_TOOL_NAME {
            ToolAuthorization::Allow
        } else {
            ToolAuthorization::Deny {
                reason: "this Runtime Host only permits echo_text in safe mode".to_owned(),
            }
        };
        Box::pin(std::future::ready(decision))
    }
}

struct UnrestrictedLocalAuthorizer {
    sessions_root: AbsolutePath,
}

impl ToolAuthorizer for UnrestrictedLocalAuthorizer {
    fn authorize<'a>(
        &'a self,
        invocation: &'a ResolvedToolInvocation,
        _batch: &'a ResolvedToolBatch,
    ) -> AuthorizationFuture<'a> {
        let decision = if let Some(facts) = invocation.facts::<FileAuthorizationFacts>() {
            if is_mutation(facts.operation) && self.is_session_attachment_path(&facts.path) {
                ToolAuthorization::Deny {
                    reason: "session attachments are static and cannot be written, edited, or deleted by structured file tools".to_owned(),
                }
            } else {
                ToolAuthorization::Allow
            }
        } else if invocation.facts::<ShellAuthorizationFacts>().is_some()
            || invocation.facts::<GeneralAuthorizationFacts>().is_some()
        {
            ToolAuthorization::Allow
        } else {
            ToolAuthorization::Deny {
                reason: "tool authorization facts are unsupported by this Runtime Host".to_owned(),
            }
        };
        Box::pin(std::future::ready(decision))
    }
}

impl UnrestrictedLocalAuthorizer {
    fn is_session_attachment_path(&self, path: &AbsolutePath) -> bool {
        let Ok(relative) = path.as_path().strip_prefix(self.sessions_root.as_path()) else {
            return false;
        };
        let mut components = relative.components();
        let Some(std::path::Component::Normal(_session_id)) = components.next() else {
            return false;
        };
        matches!(
            components.next(),
            Some(std::path::Component::Normal(name)) if name == "attachments"
        )
    }
}

fn is_mutation(operation: FileOperation) -> bool {
    matches!(
        operation,
        FileOperation::Write | FileOperation::Edit | FileOperation::Delete
    )
}

#[derive(Clone, Copy)]
struct EchoTextTool;

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
struct EchoTextInput {
    text: String,
}

#[derive(Clone, Debug, Serialize)]
struct EchoTextOutput {
    text: String,
}

impl Tool for EchoTextTool {
    type Input = EchoTextInput;
    type ResolvedInput = EchoTextInput;
    type Output = EchoTextOutput;

    fn name(&self) -> ToolName {
        ToolName::new(ECHO_TOOL_NAME).expect("static tool name is valid")
    }

    fn description(&self) -> String {
        "Return the provided text without accessing files, processes, network, or storage."
            .to_owned()
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
        Box::pin(std::future::ready(Ok(EchoTextOutput { text: input.text })))
    }
}

fn nonzero32(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("static limit is non-zero")
}

fn nonzero64(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("static limit is non-zero")
}

#[derive(Debug, Error)]
#[error("invalid local tool configuration: {message}")]
pub(crate) struct ToolResourceError {
    message: String,
}

impl ToolResourceError {
    fn configuration(error: agent_tools::ToolConfigurationError) -> Self {
        Self {
            message: error.to_string(),
        }
    }

    fn path(error: agent_tools::PathResolutionError) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use agent_model::{ModelCapabilities, SystemPromptSnapshot};
    use agent_sdk::{AgentBuilder, ContextWindowEvaluator, ExecutionInput, ExecutionOutcome};
    use agent_testkit::{ModelScript, ScriptedModelService, message_events};
    use agent_tools::{Dispatcher, ResolvedBatchItemRef};
    use agent_types::{
        AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FileReference,
        FileReferencesPart, FinishReason, MessageId, ModelIdentity, PartId, ProviderId, TextPart,
        ToolCall, ToolCallId, ToolName, ToolResultContent, UserMessage, UserPart,
    };
    use serde_json::json;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn environment(root: &TempDir, workdir: &str) -> SessionExecutionEnvironment {
        let session = root.path().join("data/sessions/session-test");
        let attachments = session.join("attachments");
        let private = session.join("private");
        std::fs::create_dir_all(&attachments).expect("attachments");
        std::fs::create_dir_all(&private).expect("private");
        SessionExecutionEnvironment {
            workspace_id: None,
            working_directory: root.path().join(workdir).to_string_lossy().into_owned(),
            workspace_private_directory: None,
            session_attachment_directory: attachments.to_string_lossy().into_owned(),
            session_private_directory: private.to_string_lossy().into_owned(),
        }
    }

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(format!("call-{name}")).expect("call id"),
            name: ToolName::new(name).expect("tool name"),
            arguments,
        }
    }

    #[test]
    fn safe_mode_exposes_only_echo_and_requires_a_live_working_directory() {
        let root = TempDir::new().expect("tempdir");
        std::fs::create_dir(root.path().join("work")).expect("workdir");
        let environment = environment(&root, "work");
        let factory = HostRunToolFactory::new(LocalToolMode::Safe, root.path()).expect("factory");
        let (tools, _) = factory
            .compile(&environment)
            .expect("safe bundle")
            .into_parts();
        assert_eq!(
            tools
                .definitions()
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            [ECHO_TOOL_NAME]
        );

        std::fs::remove_dir(root.path().join("work")).expect("remove workdir");
        let error = match factory.compile(&environment) {
            Ok(_) => panic!("missing workdir must fail"),
            Err(error) => error,
        };
        assert_eq!(
            error.kind(),
            RunToolFactoryErrorKind::WorkingDirectoryUnavailable
        );
    }

    #[tokio::test]
    async fn unrestricted_mode_reads_real_files_and_denies_structured_attachment_mutation() {
        let root = TempDir::new().expect("tempdir");
        std::fs::create_dir(root.path().join("work")).expect("workdir");
        let environment = environment(&root, "work");
        let attachment =
            std::path::Path::new(&environment.session_attachment_directory).join("reference.txt");
        std::fs::write(&attachment, "stable-token-42\n").expect("attachment");
        let factory = HostRunToolFactory::new(LocalToolMode::UnsafeUnrestricted, root.path())
            .expect("factory");
        let (tools, authorizer) = factory
            .compile(&environment)
            .expect("unrestricted bundle")
            .into_parts();
        assert_eq!(
            tools
                .definitions()
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            [
                "echo_text",
                "read_file",
                "list_directory",
                "find_files",
                "search_content",
                "write_file",
                "edit_file",
                "delete_file",
                "shell",
            ]
        );

        let mut read_batch = Dispatcher::resolve_batch(
            &tools,
            &[call(
                "read_file",
                json!({"path": attachment.to_string_lossy()}),
            )],
        );
        let ResolvedBatchItemRef::Valid(read) = read_batch.get(0).expect("read item") else {
            panic!("read resolves");
        };
        assert_eq!(
            authorizer.authorize(read, &read_batch).await,
            ToolAuthorization::Allow
        );
        let result = Dispatcher::execute(&mut read_batch, 0, ToolContext::default())
            .expect("dispatch")
            .await;
        let ToolResultContent::Json(value) = result.content else {
            panic!("read result is json");
        };
        assert!(value.to_string().contains("stable-token-42"));

        let workspace_output = root.path().join("work/output.txt");
        let mut allowed_write_batch = Dispatcher::resolve_batch(
            &tools,
            &[call(
                "write_file",
                json!({"path": workspace_output.to_string_lossy(), "content": "allowed"}),
            )],
        );
        let ResolvedBatchItemRef::Valid(allowed_write) =
            allowed_write_batch.get(0).expect("allowed write item")
        else {
            panic!("allowed write resolves");
        };
        assert_eq!(
            authorizer
                .authorize(allowed_write, &allowed_write_batch)
                .await,
            ToolAuthorization::Allow
        );
        Dispatcher::execute(&mut allowed_write_batch, 0, ToolContext::default())
            .expect("dispatch allowed write")
            .await;
        assert_eq!(
            std::fs::read_to_string(workspace_output).expect("workspace output"),
            "allowed"
        );

        let write_batch = Dispatcher::resolve_batch(
            &tools,
            &[call(
                "write_file",
                json!({"path": attachment.to_string_lossy(), "content": "changed"}),
            )],
        );
        let ResolvedBatchItemRef::Valid(write) = write_batch.get(0).expect("write item") else {
            panic!("write resolves");
        };
        assert!(matches!(
            authorizer.authorize(write, &write_batch).await,
            ToolAuthorization::Deny { .. }
        ));
        for (name, arguments) in [
            (
                "edit_file",
                json!({
                    "path": attachment.to_string_lossy(),
                    "old_string": "stable",
                    "new_string": "changed"
                }),
            ),
            ("delete_file", json!({"path": attachment.to_string_lossy()})),
        ] {
            let batch = Dispatcher::resolve_batch(&tools, &[call(name, arguments)]);
            let ResolvedBatchItemRef::Valid(invocation) = batch.get(0).expect("mutation item")
            else {
                panic!("attachment mutation resolves");
            };
            assert!(matches!(
                authorizer.authorize(invocation, &batch).await,
                ToolAuthorization::Deny { .. }
            ));
        }
        assert_eq!(
            std::fs::read_to_string(attachment).expect("unchanged attachment"),
            "stable-token-42\n"
        );

        let other_attachment = root
            .path()
            .join("data/sessions/session-other/attachments/reference.txt");
        std::fs::create_dir_all(other_attachment.parent().expect("other parent"))
            .expect("other attachments");
        std::fs::write(&other_attachment, "other-session\n").expect("other attachment");
        let other_write_batch = Dispatcher::resolve_batch(
            &tools,
            &[call(
                "write_file",
                json!({"path": other_attachment.to_string_lossy(), "content": "changed"}),
            )],
        );
        let ResolvedBatchItemRef::Valid(other_write) =
            other_write_batch.get(0).expect("other write item")
        else {
            panic!("other write resolves");
        };
        assert!(matches!(
            authorizer.authorize(other_write, &other_write_batch).await,
            ToolAuthorization::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn scripted_model_reads_the_referenced_real_file_before_completing() {
        let root = TempDir::new().expect("tempdir");
        std::fs::create_dir(root.path().join("work")).expect("workdir");
        let environment = environment(&root, "work");
        let attachment =
            std::path::Path::new(&environment.session_attachment_directory).join("reference.txt");
        std::fs::write(&attachment, "provider-visible-token-73\n").expect("attachment");
        let factory = HostRunToolFactory::new(LocalToolMode::UnsafeUnrestricted, root.path())
            .expect("factory");
        let (tools, authorizer) = factory
            .compile(&environment)
            .expect("unrestricted bundle")
            .into_parts();

        let model = Arc::new(ScriptedModelService::new(
            ModelCapabilities {
                reasoning: false,
                tool_calls: true,
                streaming: true,
            },
            8_192,
            [
                ModelScript::Events(message_events(&AssistantMessage {
                    id: MessageId::new("assistant-read").expect("message id"),
                    model: model_identity(),
                    parts: vec![AssistantPart::ToolCall(ToolCall {
                        id: ToolCallId::new("call-read").expect("call id"),
                        name: ToolName::new("read_file").expect("tool name"),
                        arguments: json!({"path": attachment.to_string_lossy()}),
                    })],
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                })),
                ModelScript::Events(message_events(&AssistantMessage {
                    id: MessageId::new("assistant-final").expect("message id"),
                    model: model_identity(),
                    parts: vec![AssistantPart::Text(TextPart {
                        id: PartId::new("answer-text").expect("part id"),
                        text: "The reference was read.".to_owned(),
                    })],
                    finish_reason: FinishReason::Stop,
                    usage: None,
                })),
            ],
        ));
        let agent = AgentBuilder::new(
            model.clone(),
            SystemPromptSnapshot::new(vec!["Read referenced files when needed.".to_owned()]),
            Arc::new(ContextWindowEvaluator::new(0.8).expect("context evaluator")),
        )
        .tools(tools)
        .build()
        .expect("agent");
        let execution = agent.start_ephemeral(
            ExecutionInput {
                conversation: ConversationSnapshot::new(vec![ConversationMessage::User(
                    UserMessage {
                        id: MessageId::new("user-files").expect("message id"),
                        parts: vec![
                            UserPart::Text(TextPart {
                                id: PartId::new("question-text").expect("part id"),
                                text: "Read this file.".to_owned(),
                            }),
                            UserPart::FileReferences(FileReferencesPart {
                                id: PartId::new("file-references").expect("part id"),
                                files: vec![FileReference {
                                    original_name: "reference.txt".to_owned(),
                                    readable_path: attachment.to_string_lossy().into_owned(),
                                }],
                            }),
                        ],
                    },
                )]),
            },
            CancellationToken::new(),
            authorizer,
        );
        assert!(matches!(
            execution.completion.await,
            ExecutionOutcome::Completed { .. }
        ));
        let requests = model.take_requests();
        assert_eq!(requests.len(), 2);
        let tool_result = requests[1]
            .conversation
            .messages
            .iter()
            .find_map(|message| match message {
                ConversationMessage::Tool(tool) => Some(&tool.result.content),
                _ => None,
            })
            .expect("second request contains tool result");
        assert!(
            serde_json::to_string(tool_result)
                .expect("tool result json")
                .contains("provider-visible-token-73")
        );
    }

    #[test]
    fn each_bundle_freezes_its_own_relative_path_resolver() {
        let root = TempDir::new().expect("root");
        std::fs::create_dir(root.path().join("first")).expect("first work");
        std::fs::create_dir(root.path().join("second")).expect("second work");
        let factory = HostRunToolFactory::new(LocalToolMode::UnsafeUnrestricted, root.path())
            .expect("factory");
        let (first_tools, _) = factory
            .compile(&environment(&root, "first"))
            .expect("first bundle")
            .into_parts();
        let (second_tools, _) = factory
            .compile(&environment(&root, "second"))
            .expect("second bundle")
            .into_parts();
        let first_batch = Dispatcher::resolve_batch(
            &first_tools,
            &[call("read_file", json!({"path": "relative.txt"}))],
        );
        let second_batch = Dispatcher::resolve_batch(
            &second_tools,
            &[call("read_file", json!({"path": "relative.txt"}))],
        );
        let ResolvedBatchItemRef::Valid(first_read) = first_batch.get(0).expect("first read")
        else {
            panic!("first resolves");
        };
        let ResolvedBatchItemRef::Valid(second_read) = second_batch.get(0).expect("second read")
        else {
            panic!("second resolves");
        };
        assert_ne!(
            first_read
                .facts::<FileAuthorizationFacts>()
                .expect("first facts")
                .path,
            second_read
                .facts::<FileAuthorizationFacts>()
                .expect("second facts")
                .path
        );
    }

    fn model_identity() -> ModelIdentity {
        ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        )
    }
}
