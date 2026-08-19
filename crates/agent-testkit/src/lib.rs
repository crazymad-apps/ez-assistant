//! Agent crate 共用的确定性测试能力。
//!
//! 所有组件完全离线运行：不访问真实 Provider、用户目录和应用数据库；
//! 时序控制只使用 gate/channel 语义，不用 sleep 判断顺序。
//!
//! - [`ScriptedModelService`] / [`ModelScript`]：脚本化模型服务，覆盖正常完成、
//!   建立前失败、流中失败和畸形事件序列注入。
//! - [`ScriptedTool`]、[`InMemoryRecorder`]、[`ScriptedPolicy`]、[`ScriptedAuthorizer`] /
//!   [`AuthorizeGate`]：引擎 Harness 的三种脚本化
//!   Fake（成功/失败/挂起工具、可注入第 N 次失败的内存 Recorder、按名决策
//!   策略、可挂起的 resolved 授权闸）；各组件共享 [`OrderLog`] 顺序日志，
//!   供断言 `begin → policy → authorize → execute → complete`。
//! - [`RecordedTransport`]：捕获出站请求并按 fixture 回放 HTTP/SSE 数据，
//!   支持建立前连接失败与响应中途断流。
//! - [`EventCollector`] / [`CollectedEvents`]：收集事件流并断言唯一终态与
//!   最终 AssistantMessage。
//! - [`CancelGate`]：在指定事件序号处精确触发取消。
//! - [`FakeFileSystemTool`] / [`FakeShellTool`]：文件与 Shell 能力的确定性 Fake，
//!   完整执行 `agent-tools` 能力契约语义（供引擎 Harness 复用）。
//! - [`FakePinnedMemoryStore`] / [`ScriptedRecallSource`] / [`ScriptedMemoryRecall`]：
//!   记忆 Store、单 Source 与统一召回能力的离线 Fake，可观察全部请求与最新状态。
//! - [`validate_request`] / [`validate_response`]：fixture 入库前的敏感信息把关。
//! - `examples/engine_demo.rs`：可直接运行的 v0.2.0 两轮工具循环效果演示。
//!
//! Fixture 约定：可审阅文本格式；不包含真实 credential、用户内容或不可再现
//! 动态值；明确协议 Adapter 和预期规范事件。
//!
//! 本 crate 只允许出现在其他 crate 的 `dev-dependencies` 中，不进入生产依赖闭包。
//!
//! 最小使用示例：`script.rs` 内联测试演示脚本化模型服务；`RecordedTransport` 等
//! 组件的真实消费见 `agent-openai-compatible` 的 `stream_tests.rs` 与
//! `deepseek_tests.rs`；引擎 Harness 见 `tests/`（行为矩阵宿主）。完整 Engine
//! 离线演示运行 `cargo run -p agent-testkit --example engine_demo`。

mod authorizer;
mod cancel;
mod collect;
mod fixture;
mod fs;
mod memory;
mod order;
mod policy;
mod record;
mod recorder;
mod script;
mod shell;
mod tool;

pub use authorizer::{AuthorizationObservation, AuthorizeGate, ScriptedAuthorizer};
pub use cancel::CancelGate;
pub use collect::{CollectedEvents, EventCollector};
pub use fixture::{FixtureViolation, validate_request, validate_response};
pub use fs::FakeFileSystemTool;
pub use memory::{
    FakePinnedMemoryStore, PinnedMemoryObservation, ScriptedMemoryRecall, ScriptedRecallSource,
};
pub use order::{LogEntry, OrderLog};
pub use policy::ScriptedPolicy;
pub use record::{
    BodyStep, RecordedRequest, RecordedResponse, RecordedTransport, RecordedTransportError,
};
pub use recorder::InMemoryRecorder;
pub use script::{ModelScript, ScriptedModelService, message_events};
pub use shell::{FakeShellCompletion, FakeShellScript, FakeShellTool};
pub use tool::{ScriptedTool, ToolExecutionGate};
