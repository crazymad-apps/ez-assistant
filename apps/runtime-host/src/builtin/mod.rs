//! 随 Host 交付的直接资源；没有安装记录、更新任务或第二份配置／连接状态。

pub(crate) mod deepseek_search;
pub(crate) mod guide;

use std::{collections::BTreeMap, io};

use assistant_protocol::{McpServerKey, McpServerSource};
use assistant_runtime::{ManagedMcpServerDefinition, SkillScanResult};
use serde_json::json;
use sha2::{Digest, Sha256};

pub(crate) fn skills() -> SkillScanResult {
    let mut result = crate::resources::embedded_skill(
        "skill-creator",
        include_str!("../../resources/builtin/skills/skill-creator/SKILL.md"),
    );
    let mut guide = crate::resources::embedded_skill(
        assistant_runtime::BUILTIN_GUIDE_SKILL_NAME,
        guide::DEFINITION,
    );
    result.complete &= guide.complete;
    result.candidates.append(&mut guide.candidates);
    result.diagnostics.append(&mut guide.diagnostics);
    result
}

/// 用户域只接纳 Host 当前可执行文件的绝对路径；用户值仍由现有 mcp.json 覆盖解析。
pub(crate) fn mcp_definitions() -> io::Result<Vec<ManagedMcpServerDefinition>> {
    let executable = std::env::current_exe()?;
    let executable = executable
        .to_str()
        .ok_or_else(|| io::Error::other("Host executable path must be UTF-8"))?;
    let raw = json!({
        "command": executable, "args":["mcp", "deepseek-search", "--stdio"],
        "displayName":"DeepSeek 搜索", "description":"实时网页搜索；在环境变量中配置 DEEPSEEK_API_KEY，主动启用并刷新后使用。",
        "enabled":false, "toolTimeoutMs":180_000,
        "env":{"DEEPSEEK_API_KEY":"", "DEEPSEEK_BASE_URL":""}
    }).as_object().cloned().expect("static builtin MCP definition is an object");
    let mut digest = Sha256::new();
    digest.update(b"builtin-mcp-definition-v1\0");
    digest.update(env!("CARGO_PKG_VERSION").as_bytes());
    digest.update(serde_json::to_vec(&raw).expect("static definition serializes"));
    Ok(vec![ManagedMcpServerDefinition {
        source: McpServerSource::Builtin,
        server_key: McpServerKey::new("deepseek-search").expect("static key"),
        definition_digest: format!("sha256-v1:{:x}", digest.finalize()),
        source_enabled: true,
        source_available: true,
        ready: true,
        raw,
        parameter_bindings: BTreeMap::new(),
        parameters: vec![],
        plugin_settings_revision: None,
        plugin_sha256: None,
        configuration: None,
    }])
}
