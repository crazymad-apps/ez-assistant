use std::{
    num::{NonZeroU32, NonZeroU64},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use super::*;
use crate::{
    AbsolutePath, Dispatcher, FileEntry, FileEntryKind, FsFuture, ResolvedBatchItemRef,
    SearchMatch, Tool, ToolRegistry,
    capability::fs::{
        DeleteFileRequest, DeleteFileResult, EditFileRequest, EditFileResult, FileSystemTool,
        FileToolContext, FileToolError, ListDirectoryRequest, ListDirectoryResult, ReadFileRequest,
        ReadFileResult, SearchFilesRequest, SearchFilesResult, WriteFileRequest, WriteFileResult,
    },
    testutil::{block_on, tool_call},
};

#[derive(Default)]
struct ProbeFs {
    reads: Mutex<Vec<ReadFileRequest>>,
}

impl FileSystemTool for ProbeFs {
    fn read<'a>(
        &'a self,
        request: ReadFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, ReadFileResult> {
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(FileToolError::Cancelled);
            }
            self.reads.lock().expect("lock reads").push(request.clone());
            Ok(ReadFileResult {
                path: request.path,
                offset: request.offset,
                limit: request.limit,
                content: "1\thello".to_owned(),
                next_offset: None,
                truncated: false,
            })
        })
    }

    fn list<'a>(
        &'a self,
        request: ListDirectoryRequest,
        _context: FileToolContext,
    ) -> FsFuture<'a, ListDirectoryResult> {
        Box::pin(async move {
            Ok(ListDirectoryResult {
                entries: vec![FileEntry {
                    path: request.path,
                    kind: FileEntryKind::Directory,
                    is_symlink: true,
                }],
            })
        })
    }

    fn search<'a>(
        &'a self,
        request: SearchFilesRequest,
        _context: FileToolContext,
    ) -> FsFuture<'a, SearchFilesResult> {
        Box::pin(async move {
            Ok(SearchFilesResult {
                matches: vec![SearchMatch::Name { path: request.path }],
                truncated: false,
                truncation_reason: None,
            })
        })
    }

    fn write<'a>(
        &'a self,
        request: WriteFileRequest,
        _context: FileToolContext,
    ) -> FsFuture<'a, WriteFileResult> {
        Box::pin(async move {
            Ok(WriteFileResult {
                path: request.path,
                bytes_written: request.content.len() as u64,
            })
        })
    }

    fn delete<'a>(
        &'a self,
        request: DeleteFileRequest,
        _context: FileToolContext,
    ) -> FsFuture<'a, DeleteFileResult> {
        Box::pin(async move {
            Ok(DeleteFileResult {
                deleted: request.path,
            })
        })
    }

    fn edit<'a>(
        &'a self,
        request: EditFileRequest,
        _context: FileToolContext,
    ) -> FsFuture<'a, EditFileResult> {
        Box::pin(async move {
            Ok(EditFileResult {
                path: request.path,
                replacements: 1,
            })
        })
    }
}

fn nonzero(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("non-zero test value")
}

fn root() -> AbsolutePath {
    #[cfg(windows)]
    let path = PathBuf::from(r"C:\workspace\project");
    #[cfg(not(windows))]
    let path = PathBuf::from("/workspace/project");
    AbsolutePath::new(path).expect("test root")
}

fn resolver() -> SessionPathResolver {
    SessionPathResolver::new(root())
}

fn read_config() -> ReadFileToolConfig {
    ReadFileToolConfig::new(nonzero(1), nonzero(200), nonzero(2000)).expect("valid read config")
}

fn search_config() -> SearchFilesToolConfig {
    SearchFilesToolConfig::new(
        nonzero(100),
        nonzero(1000),
        NonZeroU64::new(1024 * 1024).expect("non-zero"),
        NonZeroU64::new(64 * 1024).expect("non-zero"),
    )
    .expect("valid search config")
}

#[test]
fn read_schema_resolved_facts_fingerprint_and_execution_share_effective_input() {
    let fs = Arc::new(ProbeFs::default());
    let mut registry = ToolRegistry::new();
    registry
        .register(FsReadTool::new(fs.clone(), resolver(), read_config()))
        .expect("register read");
    let snapshot = registry.snapshot();
    let definition = &snapshot.definitions()[0];
    assert_eq!(
        definition.input_schema["properties"]["offset"]["default"],
        serde_json::json!(1)
    );
    assert_eq!(
        definition.input_schema["properties"]["limit"]["default"],
        serde_json::json!(200)
    );
    assert!(definition.description.contains("Calls are stateless"));
    assert!(
        definition
            .description
            .contains("never advances automatically")
    );
    assert!(
        definition
            .description
            .contains("previous result's next_offset")
    );
    assert!(
        definition.input_schema["properties"]["offset"]["description"]
            .as_str()
            .expect("offset description")
            .contains("next_offset")
    );
    let mut batch = Dispatcher::resolve_batch(
        &snapshot,
        &[tool_call(
            "read_file",
            serde_json::json!({"path": "src/../README.md"}),
        )],
    );
    let Some(ResolvedBatchItemRef::Valid(invocation)) = batch.get(0) else {
        panic!("read resolves");
    };
    let expected_path = root().as_path().join("README.md");
    assert_eq!(
        invocation.resolved_arguments(),
        &serde_json::json!({
            "path": expected_path.to_str().expect("utf8"),
            "offset": 1,
            "limit": 200,
        })
    );
    let facts = invocation
        .facts::<FileAuthorizationFacts>()
        .expect("file facts");
    assert_eq!(facts.operation, FileOperation::Read);
    assert_eq!(facts.path.as_path(), expected_path);
    assert_eq!(
        invocation.fingerprint().semantic_arguments()["operation"],
        serde_json::json!("read")
    );

    let result = block_on(
        Dispatcher::execute(&mut batch, 0, ToolContext::default()).expect("dispatch read"),
    );
    assert!(result.status == agent_types::ToolResultStatus::Success);
    assert_eq!(fs.reads.lock().expect("lock reads").len(), 1);
    assert_eq!(
        fs.reads.lock().expect("lock reads")[0].path.as_path(),
        expected_path
    );
}

#[test]
fn search_defaults_are_visible_and_limits_are_validated_during_resolve() {
    let fs = Arc::new(ProbeFs::default());
    let tool = FsFindTool::new(fs, resolver(), search_config());
    let mut registry = ToolRegistry::new();
    registry.register(tool).expect("register find");
    let snapshot = registry.snapshot();
    let definition = &snapshot.definitions()[0];
    assert_eq!(
        definition.input_schema["properties"]["path"]["default"],
        serde_json::json!(root().as_str())
    );
    assert_eq!(
        definition.input_schema["properties"]["max_results"]["default"],
        serde_json::json!(100)
    );
    let valid = Dispatcher::resolve_batch(
        &snapshot,
        &[tool_call("find_files", serde_json::json!({"query": "rs"}))],
    );
    let Some(ResolvedBatchItemRef::Valid(invocation)) = valid.get(0) else {
        panic!("find resolves");
    };
    assert_eq!(invocation.resolved_arguments()["path"], root().as_str());
    assert_eq!(invocation.resolved_arguments()["max_results"], 100);
    assert_eq!(
        invocation.resolved_arguments()["max_output_bytes"],
        1024 * 1024
    );
    assert_eq!(
        invocation.resolved_arguments()["max_record_bytes"],
        64 * 1024
    );

    let invalid = Dispatcher::resolve_batch(
        &snapshot,
        &[tool_call(
            "find_files",
            serde_json::json!({"query": "rs", "max_results": 1001}),
        )],
    );
    assert!(matches!(
        invalid.get(0),
        Some(ResolvedBatchItemRef::Invalid { .. })
    ));
}

#[test]
fn empty_paths_queries_and_invalid_edit_are_rejected_without_execution() {
    let fs = Arc::new(ProbeFs::default());
    assert!(matches!(
        FsListTool::new(fs.clone(), resolver()).resolve(ListDirectoryInput {
            path: String::new(),
        }),
        Err(ToolError::InvalidInput { .. })
    ));
    assert!(matches!(
        FsSearchTool::new(fs.clone(), resolver(), search_config()).resolve(SearchContentInput {
            query: String::new(),
            path: None,
            max_results: None,
        }),
        Err(ToolError::InvalidInput { .. })
    ));
    assert!(matches!(
        FsEditTool::new(fs, resolver()).resolve(EditFileInput {
            path: "a.txt".to_owned(),
            old_string: "same".to_owned(),
            new_string: "same".to_owned(),
            replace_all: None,
        }),
        Err(ToolError::InvalidInput { .. })
    ));
}

#[test]
fn invalid_instance_configs_fail_before_tool_construction() {
    assert!(ReadFileToolConfig::new(nonzero(1), nonzero(20), nonzero(10)).is_err());
    assert!(
        SearchFilesToolConfig::new(
            nonzero(20),
            nonzero(10),
            NonZeroU64::new(1024).expect("non-zero"),
            NonZeroU64::new(512).expect("non-zero"),
        )
        .is_err()
    );
}

#[test]
fn cancellation_is_forwarded_to_file_capability() {
    let fs = Arc::new(ProbeFs::default());
    let tool = FsReadTool::new(fs, resolver(), read_config());
    let cancellation = tokio_util::sync::CancellationToken::new();
    cancellation.cancel();
    let resolution = tool
        .resolve(ReadFileInput {
            path: "README.md".to_owned(),
            offset: None,
            limit: None,
        })
        .expect("resolve read");
    let error = block_on(tool.execute(
        resolution.into_input(),
        ToolContext::new(cancellation, Arc::new(|_| {})),
    ))
    .expect_err("cancelled capability fails");
    assert!(matches!(error, ToolError::Execution { .. }));
}

#[test]
fn mutation_tools_resolve_then_execute_through_dispatcher() {
    let fs = Arc::new(ProbeFs::default());
    let mut registry = ToolRegistry::new();
    registry
        .register(FsWriteTool::new(fs.clone(), resolver()))
        .expect("register write");
    registry
        .register(FsEditTool::new(fs.clone(), resolver()))
        .expect("register edit");
    registry
        .register(FsDeleteTool::new(fs, resolver()))
        .expect("register delete");
    let snapshot = registry.snapshot();
    let calls = [
        tool_call(
            "write_file",
            serde_json::json!({"path": "notes.txt", "content": "hello"}),
        ),
        tool_call(
            "edit_file",
            serde_json::json!({
                "path": "notes.txt",
                "old_string": "hello",
                "new_string": "world",
                "replace_all": false
            }),
        ),
        tool_call("delete_file", serde_json::json!({"path": "notes.txt"})),
    ];
    let mut batch = Dispatcher::resolve_batch(&snapshot, &calls);
    for (index, operation) in [
        FileOperation::Write,
        FileOperation::Edit,
        FileOperation::Delete,
    ]
    .into_iter()
    .enumerate()
    {
        let Some(ResolvedBatchItemRef::Valid(invocation)) = batch.get(index) else {
            panic!("mutation resolves");
        };
        assert_eq!(
            invocation
                .facts::<FileAuthorizationFacts>()
                .expect("file facts")
                .operation,
            operation
        );
        let result = block_on(
            Dispatcher::execute(&mut batch, index, ToolContext::default())
                .expect("valid mutation index"),
        );
        assert_eq!(result.status, agent_types::ToolResultStatus::Success);
    }
}
