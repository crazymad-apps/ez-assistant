//! Agent crate 共用的确定性测试能力。
//!
//! 所有组件完全离线运行：不访问真实 Provider、用户目录和应用数据库；
//! 时序控制只使用 gate/channel 语义，不用 sleep 判断顺序。
//!
//! - [`ScriptedModelService`] / [`ModelScript`]：脚本化模型服务，覆盖正常完成、
//!   建立前失败、流中失败和畸形事件序列注入。
//! - [`RecordedTransport`]：捕获出站请求并按 fixture 回放 HTTP/SSE 数据，
//!   支持建立前连接失败与响应中途断流。
//! - [`EventCollector`] / [`CollectedEvents`]：收集事件流并断言唯一终态与
//!   最终 AssistantMessage。
//! - [`CancelGate`]：在指定事件序号处精确触发取消。
//! - [`validate_request`] / [`validate_response`]：fixture 入库前的敏感信息把关。
//!
//! Fixture 约定：可审阅文本格式；不包含真实 credential、用户内容或不可再现
//! 动态值；明确 Provider profile 和预期规范事件。
//!
//! 本 crate 只允许出现在其他 crate 的 `dev-dependencies` 中，不进入生产依赖闭包。
//!
//! 最小使用示例：`script.rs` 内联测试演示脚本化模型服务；`RecordedTransport` 等
//! 组件的真实消费见 `agent-provider-openai-compatible` 的 `stream_tests.rs` 与
//! `deepseek_tests.rs`。

mod cancel;
mod collect;
mod fixture;
mod record;
mod script;

pub use cancel::CancelGate;
pub use collect::{CollectedEvents, EventCollector};
pub use fixture::{FixtureViolation, validate_request, validate_response};
pub use record::{
    BodyStep, RecordedRequest, RecordedResponse, RecordedTransport, RecordedTransportError,
};
pub use script::{ModelScript, ScriptedModelService, message_events};
