//! Agent 变体对应的稳定、可持久化用户消息注入。

use assistant_protocol::AgentVariant;

pub(crate) const PLAN_INJECTION_V1: &str = "<agent_variant mode=\"plan\">\nInvestigate the request and produce a concrete implementation plan. Use file tools and authorized shell commands when needed. You may create or modify analysis scripts and intermediate files only in the provided Agent private directories. Do not modify the user's workspace or attachments, and do not begin implementation. Shell is not a read-only sandbox and remains subject to Runtime authorization.\n</agent_variant>";

pub(crate) const BUILD_INJECTION_V1: &str = "<agent_variant mode=\"build\">\nImplement the user's request using the available tools. Follow the established plan and conversation context when applicable, verify the result, and report the completed work.\n</agent_variant>";

pub(crate) fn injection_text(variant: AgentVariant) -> &'static str {
    match variant {
        AgentVariant::Plan => PLAN_INJECTION_V1,
        AgentVariant::Build => BUILD_INJECTION_V1,
    }
}
