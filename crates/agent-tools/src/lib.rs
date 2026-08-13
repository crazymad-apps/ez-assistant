//! Agent 工具 SPI、resolved invocation、注册表、派发器与标准工具壳。
//!
//! - [`Tool`]：类型化工具抽象；serde 输入校验后执行无副作用 resolve，实际
//!   resolved input 只进入一次性执行器；[`ToolExecutionMode`] 是注册时冻结、
//!   不进入模型定义的 Core 调度属性。
//! - [`ToolRegistry`] / [`ToolSetSnapshot`]：装配期注册（重名拒绝）、执行期不可变
//!   快照；空快照是合法输入，快照可消费式追加新工具以派生稳定工具集。
//! - [`Dispatcher`] / [`ResolvedToolBatch`]：整批无副作用解析与按位置一次性执行；
//!   未知名、校验失败、resolve 失败和执行失败都形成绑定原 call ID 的结果。
//!
//! 本 crate 不实现真实文件系统或 Shell 副作用；标准工具壳的能力实现与
//! Registry 注册由 Runtime 或其他上层宿主在装配期完成。

mod capability;
mod dispatch;
mod path;
mod registry;
mod resolution;
mod standard;
mod tool;

pub use capability::{
    fs::{
        DeleteFileRequest, DeleteFileResult, EditFileRequest, EditFileResult,
        FileAuthorizationFacts, FileEntry, FileEntryKind, FileOperation, FileSystemTool,
        FileToolContext, FileToolError, FsFuture, ListDirectoryRequest, ListDirectoryResult,
        ReadFileRequest, ReadFileResult, SearchFilesRequest, SearchFilesResult, SearchKind,
        SearchMatch, SearchTruncationReason, WriteFileRequest, WriteFileResult, exact_replace,
        paginate_with_line_numbers,
    },
    shell::{
        ShellAuthorizationFacts, ShellFuture, ShellOutcome, ShellOutputChannel, ShellOutputChunk,
        ShellOutputSink, ShellProcessMode, ShellRequest, ShellTool, ShellToolError, utf8_prefix,
    },
};
pub use dispatch::{DispatchError, Dispatcher};
pub use path::{AbsolutePath, PathResolutionError, SessionPathResolver};
pub use registry::{RegisterToolError, ToolRegistry, ToolSetSnapshot};
pub use resolution::{
    GeneralAuthorizationFacts, ResolvedBatchItemRef, ResolvedToolBatch, ResolvedToolInvocation,
    ToolAuthorizationFacts, ToolFingerprint, ToolResolution,
};
pub use standard::{
    ToolConfigurationError,
    fs::{
        DeleteFileInput, EditFileInput, FindFilesInput, FsDeleteTool, FsEditTool, FsFindTool,
        FsListTool, FsReadTool, FsSearchTool, FsWriteTool, ListDirectoryInput, ReadFileInput,
        ReadFileToolConfig, SearchContentInput, SearchFilesToolConfig, WriteFileInput,
    },
    pinned_memory::{
        ListPinnedMemoriesInput, ListPinnedMemoriesTool, PinMemoryInput, PinMemoryTool,
        ResolvedUpdatePinnedMemoryInput, UnpinMemoryInput, UnpinMemoryTool,
        UpdatePinnedMemoryInput, UpdatePinnedMemoryTool,
    },
    recall_memory::{RecallMemoryInput, RecallMemoryTool, RecallMemoryToolConfig},
    shell::{ResolvedShellInput, ShellExecTool, ShellExecToolConfig, ShellInput},
};
pub use tool::{
    Tool, ToolContext, ToolError, ToolExecuteFuture, ToolExecutionMode, ToolInputDefaults,
    ToolJsonFuture, ToolOutputChannel, ToolOutputChunk, ToolOutputSink,
};

#[cfg(test)]
pub(crate) mod testutil {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };

    use agent_types::{ToolCall, ToolCallId, ToolName};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    use crate::{Tool, ToolContext, ToolError, ToolExecuteFuture};

    /// 同步驱动一个立即就绪的 Future；测试中的工具实现不允许挂起。
    pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("test future must never pend"),
        }
    }

    /// 构造一个规范 Tool Call。
    pub(crate) fn tool_call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new("call_1").expect("valid call id"),
            name: ToolName::new(name).expect("valid tool name"),
            arguments,
        }
    }

    /// 两数相加的最小工具输入。
    #[derive(Debug, Deserialize, JsonSchema, Serialize)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct AddInput {
        pub a: i64,
        pub b: i64,
    }

    /// 两数相加的最小工具输出。
    #[derive(Debug, Serialize)]
    pub(crate) struct AddOutput {
        pub sum: i64,
    }

    /// 两数相加的最小工具。
    pub(crate) struct AddTool;

    impl Tool for AddTool {
        type Input = AddInput;
        type ResolvedInput = AddInput;
        type Output = AddOutput;

        fn name(&self) -> ToolName {
            ToolName::new("add").expect("valid tool name")
        }

        fn description(&self) -> String {
            "Add two integers".to_owned()
        }

        fn resolve(
            &self,
            input: Self::Input,
        ) -> Result<crate::ToolResolution<Self::ResolvedInput>, ToolError> {
            Ok(crate::ToolResolution::general(input))
        }

        fn execute<'a>(
            &'a self,
            input: AddInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, AddOutput> {
            Box::pin(async move {
                Ok(AddOutput {
                    sum: input.a + input.b,
                })
            })
        }
    }

    /// 总是执行失败的最小工具。
    pub(crate) struct FailTool;

    impl Tool for FailTool {
        type Input = AddInput;
        type ResolvedInput = AddInput;
        type Output = AddOutput;

        fn name(&self) -> ToolName {
            ToolName::new("fail").expect("valid tool name")
        }

        fn description(&self) -> String {
            "Always fail".to_owned()
        }

        fn resolve(
            &self,
            input: Self::Input,
        ) -> Result<crate::ToolResolution<Self::ResolvedInput>, ToolError> {
            Ok(crate::ToolResolution::general(input))
        }

        fn execute<'a>(
            &'a self,
            _input: AddInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, AddOutput> {
            Box::pin(async move { Err(ToolError::execution("boom")) })
        }
    }
}
