//! 基于 Session 冻结目录为每个 Run 组装工具快照和基础设施策略。
//!
//! Host 始终为 Plan/Build 注册同一组工具定义，防止切换变体时改变 Provider 请求中的
//! tool definitions 并使提示缓存失效。这里的 policy 只表达 Host 掌握的基础设施硬限制；
//! Plan 边界、三层权限文件和交互审批统一由 `assistant-runtime` 的 Authorizer 决策。

use std::{
    ffi::OsString,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    path::Path,
    sync::Arc,
    time::Duration,
};

use agent_core::{PolicyEvaluation, ToolAuthorization, ToolPolicy};
use agent_tools::{
    AbsolutePath, FileAuthorizationFacts, FileOperation, FsDeleteTool, FsEditTool, FsFindTool,
    FsListTool, FsReadTool, FsSearchTool, FsWriteTool, ListPinnedMemoriesTool, PinMemoryTool,
    ReadFileToolConfig, RecallMemoryTool, RecallMemoryToolConfig, ResolvedToolBatch,
    ResolvedToolInvocation, SearchFilesToolConfig, SessionPathResolver, ShellExecTool,
    ShellExecToolConfig, Tool, ToolRegistry, UnpinMemoryTool, UpdatePinnedMemoryTool,
};
use agent_tools_local::{
    EnvironmentPolicy, LocalFileSystem, LocalFileSystemConfig, LocalShell, LocalShellConfig,
};
use assistant_runtime::{
    RunToolBundle, RunToolFactory, RunToolFactoryError, RunToolFactoryErrorKind,
    RunToolFactoryRequest, SessionExecutionEnvironment, pinned_memory_limits,
};
use thiserror::Error;

const MAX_TEXT_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SEARCH_OUTPUT_BYTES: u64 = 1024 * 1024;
const MAX_SEARCH_RECORD_BYTES: u64 = 64 * 1024;
const MAX_SEARCH_STDERR_BYTES: u64 = 64 * 1024;
const MAX_SHELL_OUTPUT_BYTES: u64 = 1024 * 1024;

pub(super) struct HostRunToolFactory {
    resources: LocalToolResources,
    sessions_root: AbsolutePath,
}

struct LocalToolResources {
    filesystem: Arc<LocalFileSystem>,
    shell: Arc<LocalShell>,
    read_config: ReadFileToolConfig,
    search_config: SearchFilesToolConfig,
    shell_config: ShellExecToolConfig,
}

impl HostRunToolFactory {
    pub(super) fn new(runtime_home: &Path) -> Result<Self, ToolResourceError> {
        let sessions_root = AbsolutePath::new(runtime_home.join("data/sessions"))
            .map_err(ToolResourceError::path)?;
        Ok(Self {
            resources: LocalToolResources::new()?,
            sessions_root,
        })
    }
}

impl RunToolFactory for HostRunToolFactory {
    fn compile(
        &self,
        request: RunToolFactoryRequest<'_>,
    ) -> Result<RunToolBundle, RunToolFactoryError> {
        let resolver = checked_resolver(request.environment)?;
        self.resources
            .compile(request, resolver, self.sessions_root.clone())
    }
}

impl LocalToolResources {
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
        request: RunToolFactoryRequest<'_>,
        resolver: SessionPathResolver,
        sessions_root: AbsolutePath,
    ) -> Result<RunToolBundle, RunToolFactoryError> {
        // 注册顺序也是 Provider 可见请求的一部分，保持固定顺序可让相同工具集稳定序列化。
        let mut registry = ToolRegistry::new();
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
        let limits = pinned_memory_limits();
        register(
            &mut registry,
            ListPinnedMemoriesTool::new(request.pinned_memory.clone()),
        )?;
        register(
            &mut registry,
            PinMemoryTool::new(request.pinned_memory.clone(), limits.clone()),
        )?;
        register(
            &mut registry,
            UpdatePinnedMemoryTool::new(request.pinned_memory.clone(), limits.clone()),
        )?;
        register(
            &mut registry,
            UnpinMemoryTool::new(request.pinned_memory, limits),
        )?;
        register(
            &mut registry,
            RecallMemoryTool::new(
                request.conversation_recall,
                RecallMemoryToolConfig::new(
                    NonZeroUsize::new(20).expect("static recall limit is non-zero"),
                ),
            )
            .with_reference_reader(request.conversation_recall_reader),
        )?;
        // 校验当前 Session 冻结附件目录的类型边界。Authorizer 持有
        // Runtime Home 下的 sessions root，因此同样保护其他 Session 附件。
        AbsolutePath::new(&request.environment.session_attachment_directory).map_err(|source| {
            RunToolFactoryError::with_source(RunToolFactoryErrorKind::InvalidConfiguration, source)
        })?;
        Ok(RunToolBundle::new(
            registry.snapshot(),
            vec![
                Arc::new(AttachmentMutationPolicy {
                    sessions_root: sessions_root.clone(),
                }),
                Arc::new(SessionPermissionFileMutationPolicy { sessions_root }),
            ],
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

fn register<T: Tool>(registry: &mut ToolRegistry, tool: T) -> Result<(), RunToolFactoryError> {
    registry.register(tool).map_err(|source| {
        RunToolFactoryError::with_source(RunToolFactoryErrorKind::InvalidConfiguration, source)
    })
}

struct AttachmentMutationPolicy {
    sessions_root: AbsolutePath,
}

impl ToolPolicy for AttachmentMutationPolicy {
    fn evaluate(
        &self,
        invocation: &ResolvedToolInvocation,
        _batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        // 附件是 Session 的静态参考文件。只阻止结构化文件工具修改；Shell 仍以当前用户
        // 权限运行，所以这是一条应用策略，不应被描述成 OS 沙箱或不可绕过隔离。
        if let Some(facts) = invocation.facts::<FileAuthorizationFacts>() {
            if is_mutation(facts.operation) && self.is_session_attachment_path(&facts.path) {
                PolicyEvaluation::Decide(ToolAuthorization::Deny {
                    reason: "session attachments are static and cannot be written, edited, or deleted by structured file tools".to_owned(),
                })
            } else {
                PolicyEvaluation::Continue
            }
        } else {
            PolicyEvaluation::Continue
        }
    }
}

impl AttachmentMutationPolicy {
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

/// Session 权限文件位于 Agent 可写的私有目录中，但它是 Host 控制面文件，不是普通业务文件。
/// 防止结构化文件工具借助默认私有目录写权限修改自己的授权边界。
struct SessionPermissionFileMutationPolicy {
    sessions_root: AbsolutePath,
}

impl ToolPolicy for SessionPermissionFileMutationPolicy {
    fn evaluate(
        &self,
        invocation: &ResolvedToolInvocation,
        _batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        if let Some(facts) = invocation.facts::<FileAuthorizationFacts>() {
            if is_mutation(facts.operation) && self.is_session_permission_file(&facts.path) {
                PolicyEvaluation::Decide(ToolAuthorization::Deny {
                    reason: "session permissions are managed by the Runtime and cannot be modified by structured file tools".to_owned(),
                })
            } else {
                PolicyEvaluation::Continue
            }
        } else {
            PolicyEvaluation::Continue
        }
    }
}

impl SessionPermissionFileMutationPolicy {
    fn is_session_permission_file(&self, path: &AbsolutePath) -> bool {
        let Ok(relative) = path.as_path().strip_prefix(self.sessions_root.as_path()) else {
            return false;
        };
        let mut components = relative.components();
        let Some(std::path::Component::Normal(_session_id)) = components.next() else {
            return false;
        };
        matches!(
            (
                components.next(),
                components.next(),
                components.next()
            ),
            (
                Some(std::path::Component::Normal(private)),
                Some(std::path::Component::Normal(permission_file)),
                None
            ) if private == "private" && permission_file == "permissions.json"
        )
    }
}

fn is_mutation(operation: FileOperation) -> bool {
    matches!(
        operation,
        FileOperation::Write | FileOperation::Edit | FileOperation::Delete
    )
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
    use agent_core::{AllowAllAuthorizer, ComposedToolAuthorizer, ToolAuthorizer};
    use agent_memory::MemoryRecallResponse;
    use agent_model::{
        GenerationConfig, ModelCapabilities, ModelRequest, ProviderOptions, SystemPromptSnapshot,
    };
    use agent_provider_openai_compatible::{Profile, encode_request};
    use agent_sdk::{AgentBuilder, ContextWindowEvaluator, ExecutionInput, ExecutionOutcome};
    use agent_testkit::{
        FakePinnedMemoryStore, ModelScript, ScriptedMemoryRecall, ScriptedModelService,
        message_events,
    };
    use agent_tools::{Dispatcher, ResolvedBatchItemRef, ToolContext};
    use agent_types::{
        AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FileReference,
        FileReferencesPart, FinishReason, MessageId, ModelIdentity, PartId, ProviderId, TextPart,
        ToolCall, ToolCallId, ToolChoice, ToolName, ToolResultContent, UserMessage, UserPart,
    };
    use serde_json::json;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn compile_bundle(
        factory: &HostRunToolFactory,
        environment: &SessionExecutionEnvironment,
    ) -> RunToolBundle {
        let session_id = assistant_protocol::SessionId::new("session-test").expect("session id");
        let conversation_recall = Arc::new(ScriptedMemoryRecall::new(Ok(MemoryRecallResponse {
            items: Vec::new(),
            failures: Vec::new(),
            truncated: false,
            window: None,
        })));
        factory
            .compile(RunToolFactoryRequest {
                session_id: &session_id,
                environment,
                pinned_memory: Arc::new(FakePinnedMemoryStore::new(Vec::new())),
                conversation_recall: conversation_recall.clone(),
                conversation_recall_reader: conversation_recall,
            })
            .expect("tool bundle")
    }

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

    #[tokio::test]
    async fn default_bundle_reads_real_files_and_denies_managed_session_storage_mutation() {
        let root = TempDir::new().expect("tempdir");
        std::fs::create_dir(root.path().join("work")).expect("workdir");
        let environment = environment(&root, "work");
        let attachment =
            std::path::Path::new(&environment.session_attachment_directory).join("reference.txt");
        std::fs::write(&attachment, "stable-token-42\n").expect("attachment");
        let factory = HostRunToolFactory::new(root.path()).expect("factory");
        let (tools, policies) = compile_bundle(&factory, &environment).into_parts();
        let authorizer = test_authorizer(policies);
        assert_eq!(
            tools
                .definitions()
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            [
                "read_file",
                "list_directory",
                "find_files",
                "search_content",
                "write_file",
                "edit_file",
                "delete_file",
                "shell",
                "list_pinned_memories",
                "pin_memory",
                "update_pinned_memory",
                "unpin_memory",
                "recall_memory",
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

        let permission_file =
            std::path::Path::new(&environment.session_private_directory).join("permissions.json");
        std::fs::write(&permission_file, r#"{"schema_version":1,"rules":[]}"#)
            .expect("permission file");
        let permission_write_batch = Dispatcher::resolve_batch(
            &tools,
            &[call(
                "write_file",
                json!({
                    "path": permission_file.to_string_lossy(),
                    "content": r#"{"schema_version":1,"rules":[{"id":"self-grant"}]}"#
                }),
            )],
        );
        let ResolvedBatchItemRef::Valid(permission_write) = permission_write_batch
            .get(0)
            .expect("permission write item")
        else {
            panic!("permission write resolves");
        };
        assert!(matches!(
            authorizer
                .authorize(permission_write, &permission_write_batch)
                .await,
            ToolAuthorization::Deny { .. }
        ));
    }

    #[test]
    fn every_host_tool_schema_encodes_for_the_deepseek_function_subset() {
        let root = TempDir::new().expect("tempdir");
        std::fs::create_dir(root.path().join("work")).expect("workdir");
        let environment = environment(&root, "work");
        let factory = HostRunToolFactory::new(root.path()).expect("factory");
        let (tools, _) = compile_bundle(&factory, &environment).into_parts();
        let request = ModelRequest {
            system: SystemPromptSnapshot::default(),
            conversation: ConversationSnapshot::new(Vec::new()),
            tools: tools.definitions().to_vec(),
            tool_choice: ToolChoice::Auto,
            generation: GenerationConfig::default(),
            reasoning: None,
            provider_options: ProviderOptions::new(),
        };

        // 用真实 Host Bundle 审计，而不是只验证 recall_memory 的手写样例。以后标准
        // 工具新增方言敏感 Schema 时，这里会在请求编码阶段给出具体工具名。
        let encoded = encode_request(&request, &Profile::deepseek(), "deepseek-chat")
            .expect("every registered host tool must encode for DeepSeek");
        assert_eq!(
            encoded.tools.as_ref().map(Vec::len),
            Some(tools.definitions().len())
        );
        let encoded = serde_json::to_value(encoded).expect("encoded request");
        assert_provider_tool_subset(&encoded["tools"]);
    }

    fn assert_provider_tool_subset(value: &serde_json::Value) {
        match value {
            serde_json::Value::Array(values) => {
                for value in values {
                    assert_provider_tool_subset(value);
                }
            }
            serde_json::Value::Object(object) => {
                for forbidden in [
                    "$schema",
                    "$defs",
                    "definitions",
                    "$ref",
                    "oneOf",
                    "const",
                    "default",
                ] {
                    assert!(
                        !object.contains_key(forbidden),
                        "Provider tool schema still contains `{forbidden}` in {value}"
                    );
                }
                for value in object.values() {
                    assert_provider_tool_subset(value);
                }
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn scripted_model_reads_the_referenced_real_file_before_completing() {
        let root = TempDir::new().expect("tempdir");
        std::fs::create_dir(root.path().join("work")).expect("workdir");
        let environment = environment(&root, "work");
        let attachment =
            std::path::Path::new(&environment.session_attachment_directory).join("reference.txt");
        std::fs::write(&attachment, "provider-visible-token-73\n").expect("attachment");
        let factory = HostRunToolFactory::new(root.path()).expect("factory");
        let (tools, policies) = compile_bundle(&factory, &environment).into_parts();
        let authorizer = test_authorizer(policies);

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
        let factory = HostRunToolFactory::new(root.path()).expect("factory");
        let first_environment = environment(&root, "first");
        let second_environment = environment(&root, "second");
        let (first_tools, _) = compile_bundle(&factory, &first_environment).into_parts();
        let (second_tools, _) = compile_bundle(&factory, &second_environment).into_parts();
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

    #[test]
    fn repeated_compilation_keeps_tool_definitions_byte_equivalent() {
        let root = TempDir::new().expect("root");
        std::fs::create_dir(root.path().join("work")).expect("workdir");
        let factory = HostRunToolFactory::new(root.path()).expect("factory");
        let environment = environment(&root, "work");
        let (first, _) = compile_bundle(&factory, &environment).into_parts();
        let (second, _) = compile_bundle(&factory, &environment).into_parts();
        assert_eq!(
            serde_json::to_vec(first.definitions()).expect("first definitions"),
            serde_json::to_vec(second.definitions()).expect("second definitions")
        );
    }

    fn model_identity() -> ModelIdentity {
        ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        )
    }

    fn test_authorizer(policies: Vec<Arc<dyn ToolPolicy>>) -> Arc<dyn ToolAuthorizer> {
        Arc::new(ComposedToolAuthorizer::new(
            policies,
            Arc::new(AllowAllAuthorizer),
        ))
    }
}
