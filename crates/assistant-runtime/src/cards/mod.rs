//! 消息卡片的入口准入、发布和最新快照；业务事实归 Runtime 与 Store。

mod manifest;
mod publisher;
pub(crate) use publisher::{CardAuthorizationFacts, CardPublisher, PublishCardTool};

pub use manifest::validate_plugin_ui;
