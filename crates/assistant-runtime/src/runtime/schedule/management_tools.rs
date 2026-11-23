//! 仅用户发起的父 Run 可管理当前 Runtime 的计划。工具输入不能选择其他用户或设备。

use std::sync::{Arc, Weak};

use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};
use agent_types::ToolName;
use assistant_protocol as protocol;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{AssistantRuntime, RuntimeError, session::SessionController};

const TOOL_NAME: &str = "manage_schedules";

#[derive(Clone)]
pub(crate) struct ScheduleManagementFacts {
    pub(crate) write: bool,
}

pub(in crate::runtime) struct ScheduleTool {
    runtime: Weak<AssistantRuntime>,
    session: Arc<SessionController>,
    run_id: protocol::RunId,
}

impl ScheduleTool {
    pub(in crate::runtime) fn new(
        runtime: Weak<AssistantRuntime>,
        session: Arc<SessionController>,
        run_id: protocol::RunId,
    ) -> Self {
        Self {
            runtime,
            session,
            run_id,
        }
    }
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::runtime) struct Page {
    offset: Option<u32>,
    limit: Option<u32>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Rule {
    Once {
        date: String,
        time: String,
    },
    Daily {
        time: String,
    },
    Weekly {
        weekdays: Vec<Weekday>,
        time: String,
    },
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ApprovalMode {
    Ask,
    Auto,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ModelSource {
    Local,
    Organization,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Model {
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<ModelSource>,
    provider_instance_id: String,
    model_id: String,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CustomAgent {
    name: String,
    source: CustomAgentSource,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum CustomAgentSource {
    Personal,
    Center {
        center_id: String,
        organization_id: Option<String>,
    },
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Script {
        command: String,
    },
    Agent {
        instructions: String,
        custom_agent: Option<CustomAgent>,
        model_selection: Option<Model>,
        approval_mode: ApprovalMode,
    },
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::runtime) struct Settings {
    name: String,
    enabled: bool,
    workspace_id: Option<String>,
    rule: Rule,
    action: Action,
    report_to_controller: Option<bool>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::runtime) enum Input {
    List {
        page: Option<Page>,
    },
    Get {
        schedule_id: String,
    },
    Create {
        settings: Settings,
    },
    Update {
        schedule_id: String,
        settings: Settings,
        expected_settings: Option<Box<Settings>>,
    },
    SetEnabled {
        schedule_id: String,
        enabled: bool,
    },
    RunNow {
        schedule_id: String,
    },
    Delete {
        schedule_id: String,
    },
    ListExecutions {
        schedule_id: String,
        page: Option<Page>,
    },
    GetExecution {
        execution_id: String,
    },
}

#[derive(Serialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub(in crate::runtime) enum Request {
    List(protocol::ListSchedulesRequest),
    Get(protocol::ScheduleTarget),
    Create(protocol::CreateScheduleRequest),
    Update(protocol::UpdateScheduleRequest),
    SetEnabled(protocol::SetScheduleEnabledRequest),
    RunNow(protocol::ScheduleTarget),
    Delete(protocol::ScheduleTarget),
    ListExecutions(protocol::ListScheduleExecutionsRequest),
    GetExecution(protocol::ScheduleExecutionTarget),
}

fn convert<T: DeserializeOwned>(value: impl Serialize) -> Result<T, ToolError> {
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .map_err(|error| ToolError::invalid_input(error.to_string()))
}

fn schedule_target(id: String) -> Result<protocol::ScheduleTarget, ToolError> {
    Ok(protocol::ScheduleTarget {
        schedule_id: protocol::ScheduleId::new(id)
            .map_err(|_| ToolError::invalid_input("invalid schedule_id"))?,
    })
}

impl Tool for ScheduleTool {
    type Input = Input;
    type ResolvedInput = Request;
    type Output = Value;

    fn name(&self) -> ToolName {
        ToolName::new(TOOL_NAME).expect("static tool name")
    }

    fn description(&self) -> String {
        "Manage scheduled tasks on this conversation's Host. Read with list/get/list_executions/get_execution. Only change a schedule when requested by the user. Calendar dates use YYYY-MM-DD, times HH:MM on the execution device. Create with an Agent or exact Shell command and optional existing workspace_id. report_to_controller defaults to true on create and is required on update. Get the latest definition before a full update and preserve unrelated settings; use set_enabled for pause/resume. Run_now can join an already running execution. Delete stops execution and removes its history. Scheduled Agent tasks cannot ask for interactive approval.".into()
    }

    fn resolve(&self, input: Input) -> Result<ToolResolution<Request>, ToolError> {
        let request = match input {
            Input::List { page } => Request::List(convert(json!({ "page": page }))?),
            Input::Get { schedule_id } => Request::Get(schedule_target(schedule_id)?),
            Input::Create { settings } => Request::Create(convert(settings)?),
            Input::Update {
                schedule_id,
                settings,
                expected_settings,
            } => {
                if settings.report_to_controller.is_none() {
                    return Err(ToolError::invalid_input(
                        "report_to_controller is required on update",
                    ));
                }
                Request::Update(protocol::UpdateScheduleRequest {
                    schedule_id: schedule_target(schedule_id)?.schedule_id,
                    settings: convert(settings)?,
                    expected_settings: expected_settings
                        .map(|settings| convert(*settings).map(Box::new))
                        .transpose()?,
                })
            }
            Input::SetEnabled {
                schedule_id,
                enabled,
            } => Request::SetEnabled(protocol::SetScheduleEnabledRequest {
                schedule_id: schedule_target(schedule_id)?.schedule_id,
                enabled,
            }),
            Input::RunNow { schedule_id } => Request::RunNow(schedule_target(schedule_id)?),
            Input::Delete { schedule_id } => Request::Delete(schedule_target(schedule_id)?),
            Input::ListExecutions { schedule_id, page } => {
                Request::ListExecutions(convert(json!({
                    "schedule_id": schedule_id,
                    "page": page,
                }))?)
            }
            Input::GetExecution { execution_id } => Request::GetExecution(convert(json!({
                "execution_id": execution_id,
            }))?),
        };
        let write = !matches!(
            request,
            Request::List(_)
                | Request::Get(_)
                | Request::ListExecutions(_)
                | Request::GetExecution(_)
        );
        let projection = serde_json::to_value(&request)
            .map_err(|_| ToolError::invalid_input("schedule request could not be encoded"))?;
        Ok(ToolResolution::with_facts(
            request,
            ScheduleManagementFacts { write },
            projection,
        ))
    }

    fn execute<'a>(
        &'a self,
        request: Request,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Value> {
        Box::pin(async move {
            self.session.ensure_active().map_err(safe_error)?;
            let call = context
                .call_id()
                .ok_or_else(|| ToolError::execution("schedule call identity is unavailable"))?;
            let pending = self
                .session
                .lock_state()
                .map_err(safe_error)?
                .journal
                .as_ref()
                .is_some_and(|journal| {
                    journal.has_pending_tool_call(&self.run_id, call, TOOL_NAME)
                });
            if !pending || context.cancellation.is_cancelled() {
                return Err(ToolError::execution(
                    "schedule operation requires a live tool call",
                ));
            }
            let runtime = self
                .runtime
                .upgrade()
                .ok_or_else(|| ToolError::execution("schedule Runtime is unavailable"))?;
            match request {
                Request::List(request) => encode(runtime.list_schedules(request).await),
                Request::Get(request) => encode(runtime.get_schedule(request).await),
                Request::Create(request) => encode(runtime.create_schedule(request).await),
                Request::Update(request) => encode(runtime.update_schedule(request).await),
                Request::SetEnabled(request) => encode(runtime.set_schedule_enabled(request).await),
                Request::RunNow(target) => {
                    let mut digest = Sha256::new();
                    digest.update(self.run_id.as_str().as_bytes());
                    digest.update([0]);
                    digest.update(call.as_str().as_bytes());
                    let idempotency_key =
                        protocol::IdempotencyKey::new(format!("chat-{:x}", digest.finalize()))
                            .map_err(|_| {
                                ToolError::execution("schedule execution identity is unavailable")
                            })?;
                    let plan = runtime
                        .get_schedule(target.clone())
                        .await
                        .map_err(safe_error)?;
                    let execution = runtime
                        .run_schedule_now(protocol::RunScheduleNowRequest {
                            schedule_id: target.schedule_id,
                            idempotency_key,
                        })
                        .await
                        .map_err(safe_error)?;
                    Ok(
                        json!({"name": plan.settings.configuration.name, "next_run_at": plan.next_run_at, "result": execution}),
                    )
                }
                Request::Delete(request) => {
                    let plan = runtime
                        .get_schedule(request.clone())
                        .await
                        .map_err(safe_error)?;
                    let deleted = runtime.delete_schedule(request).await.map_err(safe_error)?;
                    Ok(
                        json!({"schedule_id": deleted.schedule_id, "name": plan.settings.configuration.name, "next_run_at": null, "deleted": true}),
                    )
                }
                Request::ListExecutions(request) => {
                    encode(runtime.list_schedule_executions(request).await)
                }
                Request::GetExecution(request) => {
                    encode(runtime.get_schedule_execution(request).await)
                }
            }
        })
    }
}

fn encode(result: Result<impl Serialize, RuntimeError>) -> Result<Value, ToolError> {
    serde_json::to_value(result.map_err(safe_error)?)
        .map_err(|_| ToolError::execution("schedule response could not be encoded"))
}

fn safe_error(error: RuntimeError) -> ToolError {
    ToolError::execution(error.to_protocol_info().message)
}
