//! Agent 管理工具只接受任务标识，用户域和会话由父 Run 冻结。

use agent_tools::{
    Tool, ToolContext, ToolError, ToolExecuteFuture, ToolInputDefaults, ToolRegistry,
    ToolResolution, ToolSetSnapshot,
};
use agent_types::ToolName;
use assistant_protocol::{
    BackgroundRunStatus, ListBackgroundRunsRequest, ListBackgroundRunsResult,
    ReadBackgroundOutputRequest, ReadBackgroundOutputResult, SessionId, StopBackgroundRunRequest,
    StopBackgroundRunResult,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::BackgroundRunServices;

pub(crate) struct BackgroundManagementFacts {
    pub(crate) write: bool,
}

#[derive(Clone)]
struct Context {
    services: BackgroundRunServices,
    session_id: SessionId,
}

pub(in crate::runtime) fn tool_set(
    services: BackgroundRunServices,
    session_id: SessionId,
) -> Result<ToolSetSnapshot, agent_tools::RegisterToolError> {
    let context = Context {
        services,
        session_id,
    };
    let mut registry = ToolRegistry::new();
    registry.register(List(context.clone()))?;
    registry.register(Read(context.clone()))?;
    registry.register(Stop(context))?;
    Ok(registry.snapshot())
}

struct List(Context);
struct Read(Context);
struct Stop(Context);

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListInput {
    offset: Option<u32>,
    limit: Option<u32>,
    status: Option<Status>,
    background_id: Option<String>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Status {
    Running,
    CleanupUnconfirmed,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    background_id: String,
    cursor: Option<u64>,
    offset_chars: Option<i64>,
    limit_chars: Option<u32>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StopInput {
    background_id: String,
}

fn resolved<I: Serialize>(input: I, write: bool) -> ToolResolution<I> {
    let semantic = serde_json::to_value(&input).expect("background arguments serialize");
    ToolResolution::with_facts(input, BackgroundManagementFacts { write }, semantic)
}

impl Tool for List {
    type Input = ListInput;
    type ResolvedInput = ListInput;
    type Output = ListBackgroundRunsResult;

    fn name(&self) -> ToolName {
        ToolName::new("list_background_runs").expect("static tool name")
    }

    fn description(&self) -> String {
        "List active background commands in this session, optionally filtered by ID or status. Read output separately. Offset starts at zero and limit is at most 50.".into()
    }

    fn input_defaults(&self) -> ToolInputDefaults {
        ToolInputDefaults::new().with("offset", 0).with("limit", 10)
    }

    fn resolve(
        &self,
        mut input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        input.offset = Some(input.offset.unwrap_or(0));
        input.limit = Some(input.limit.unwrap_or(10));
        Ok(resolved(input, false))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        _: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.0
                .services
                .runs()
                .list(ListBackgroundRunsRequest {
                    session_id: self.0.session_id.clone(),
                    offset: input.offset,
                    limit: input.limit,
                    status: input.status.map(|status| match status {
                        Status::Running => BackgroundRunStatus::Running,
                        Status::CleanupUnconfirmed => BackgroundRunStatus::CleanupUnconfirmed,
                    }),
                    background_id: input.background_id,
                    include_command: None,
                })
                .map_err(tool_error)
        })
    }
}

impl Tool for Read {
    type Input = ReadInput;
    type ResolvedInput = ReadInput;
    type Output = ReadBackgroundOutputResult;

    fn name(&self) -> ToolName {
        ToolName::new("read_background_output").expect("static tool name")
    }

    fn description(&self) -> String {
        "Read up to 8000 Unicode characters from an active background command by ID. Omit cursor to read the latest page and use next_cursor to continue. offset_chars is signed and requires a cursor when nonzero. dropped_chars reports output lost to rotation.".into()
    }

    fn input_defaults(&self) -> ToolInputDefaults {
        ToolInputDefaults::new()
            .with("offset_chars", 0)
            .with("limit_chars", 2_000)
    }

    fn resolve(
        &self,
        mut input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        input.offset_chars = Some(input.offset_chars.unwrap_or(0));
        input.limit_chars = Some(input.limit_chars.unwrap_or(2_000));
        Ok(resolved(input, false))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        _: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.0
                .services
                .runs()
                .read(ReadBackgroundOutputRequest {
                    session_id: self.0.session_id.clone(),
                    background_id: input.background_id,
                    cursor: input.cursor,
                    offset_chars: input.offset_chars,
                    limit_chars: input.limit_chars,
                })
                .await
                .map_err(tool_error)
        })
    }
}

impl Tool for Stop {
    type Input = StopInput;
    type ResolvedInput = StopInput;
    type Output = StopBackgroundRunResult;

    fn name(&self) -> ToolName {
        ToolName::new("stop_background_run").expect("static tool name")
    }

    fn description(&self) -> String {
        "Stop an active background command in this session by ID and wait for its exit result. Requires write permission. Output is removed when cleanup completes.".into()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        Ok(resolved(input, true))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(ToolError::execution("background stop cancelled"));
            }
            self.0
                .services
                .stop(StopBackgroundRunRequest {
                    session_id: self.0.session_id.clone(),
                    background_id: input.background_id,
                })
                .await
                .map_err(tool_error)
        })
    }
}

fn tool_error(error: crate::RuntimeError) -> ToolError {
    let info = error.to_protocol_info();
    ToolError::execution_with_details(
        info.message.clone(),
        serde_json::json!({ "code": info.code }),
    )
}
