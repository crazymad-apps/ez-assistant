//! Core Demo 私有的真实本地工具装配。

use std::{
    ffi::OsString,
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
    time::Duration,
};

use agent_tools::{
    AbsolutePath, FsDeleteTool, FsEditTool, FsFindTool, FsListTool, FsReadTool, FsSearchTool,
    FsWriteTool, ListPinnedMemoriesTool, PinMemoryTool, ReadFileToolConfig, RecallMemoryTool,
    RecallMemoryToolConfig, SearchFilesToolConfig, SessionPathResolver, ShellExecTool,
    ShellExecToolConfig, ToolRegistry, ToolSetSnapshot, UnpinMemoryTool, UpdatePinnedMemoryTool,
};
use agent_tools_local::{
    EnvironmentPolicy, LocalFileSystem, LocalFileSystemConfig, LocalShell, LocalShellConfig,
};
use thiserror::Error;

use crate::memory::DemoMemoryResources;

const MAX_TEXT_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SEARCH_OUTPUT_BYTES: u64 = 1024 * 1024;
const MAX_SEARCH_RECORD_BYTES: u64 = 64 * 1024;
const MAX_SEARCH_STDERR_BYTES: u64 = 64 * 1024;
const MAX_SHELL_OUTPUT_BYTES: u64 = 1024 * 1024;

/// 为一个冻结的 Session 工作目录创建标准文件和 Shell 工具集合。
pub(crate) fn build_tools(
    workdir: &AbsolutePath,
    memory: &DemoMemoryResources,
) -> Result<ToolSetSnapshot, ToolingError> {
    let resolver = SessionPathResolver::new(workdir.clone());
    let filesystem = Arc::new(LocalFileSystem::new(LocalFileSystemConfig {
        max_text_file_bytes: nonzero64(MAX_TEXT_FILE_BYTES),
        ripgrep_program: OsString::from("rg"),
        max_search_stderr_bytes: nonzero64(MAX_SEARCH_STDERR_BYTES),
    }));
    let read_config = ReadFileToolConfig::new(nonzero32(1), nonzero32(200), nonzero32(2_000))
        .map_err(tool_config_error)?;
    let search_config = SearchFilesToolConfig::new(
        nonzero32(100),
        nonzero32(1_000),
        nonzero64(MAX_SEARCH_OUTPUT_BYTES),
        nonzero64(MAX_SEARCH_RECORD_BYTES),
    )
    .map_err(tool_config_error)?;
    let shell = Arc::new(LocalShell::new(LocalShellConfig::new(
        EnvironmentPolicy::default(),
    )));
    let shell_config = ShellExecToolConfig::new(
        Duration::from_secs(30),
        Duration::from_secs(120),
        nonzero64(MAX_SHELL_OUTPUT_BYTES),
    )
    .map_err(tool_config_error)?;

    let mut registry = ToolRegistry::new();
    registry
        .register(FsReadTool::new(
            filesystem.clone(),
            resolver.clone(),
            read_config,
        ))
        .map_err(register_error)?;
    registry
        .register(FsListTool::new(filesystem.clone(), resolver.clone()))
        .map_err(register_error)?;
    registry
        .register(FsFindTool::new(
            filesystem.clone(),
            resolver.clone(),
            search_config,
        ))
        .map_err(register_error)?;
    registry
        .register(FsSearchTool::new(
            filesystem.clone(),
            resolver.clone(),
            search_config,
        ))
        .map_err(register_error)?;
    registry
        .register(FsWriteTool::new(filesystem.clone(), resolver.clone()))
        .map_err(register_error)?;
    registry
        .register(FsEditTool::new(filesystem.clone(), resolver.clone()))
        .map_err(register_error)?;
    registry
        .register(FsDeleteTool::new(filesystem, resolver.clone()))
        .map_err(register_error)?;
    registry
        .register(ShellExecTool::new(shell, resolver, shell_config))
        .map_err(register_error)?;
    let store: Arc<dyn agent_memory::PinnedMemoryStore> = memory.store.clone();
    registry
        .register(PinMemoryTool::new(store.clone(), memory.limits.clone()))
        .map_err(register_error)?;
    registry
        .register(UpdatePinnedMemoryTool::new(
            store.clone(),
            memory.limits.clone(),
        ))
        .map_err(register_error)?;
    registry
        .register(UnpinMemoryTool::new(store.clone(), memory.limits.clone()))
        .map_err(register_error)?;
    registry
        .register(ListPinnedMemoriesTool::new(store))
        .map_err(register_error)?;
    registry
        .register(RecallMemoryTool::new(
            memory.recall.clone(),
            RecallMemoryToolConfig::new(
                std::num::NonZeroUsize::new(20).expect("static limit is non-zero"),
            ),
        ))
        .map_err(register_error)?;
    Ok(registry.snapshot())
}

fn nonzero32(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("static limit is non-zero")
}

fn nonzero64(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("static limit is non-zero")
}

fn tool_config_error(error: agent_tools::ToolConfigurationError) -> ToolingError {
    ToolingError::Configuration(error.to_string())
}

fn register_error(error: agent_tools::RegisterToolError) -> ToolingError {
    ToolingError::Configuration(error.to_string())
}

#[derive(Debug, Error)]
pub(crate) enum ToolingError {
    #[error("invalid local tool configuration: {0}")]
    Configuration(String),
}
