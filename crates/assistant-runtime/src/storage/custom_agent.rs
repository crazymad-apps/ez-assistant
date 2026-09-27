//! 个人自定义智能体的完整配置与内容摘要；中心目录不在本地复制。

use std::collections::BTreeSet;

use assistant_protocol::{AgentCapabilityReference, CustomAgentConfig, McpServerKey};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::SkillName;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredCustomAgent {
    pub config: CustomAgentConfig,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CustomAgentMutation {
    Save {
        expected_revision: Option<String>,
        next: StoredCustomAgent,
    },
    Delete {
        name: String,
        expected_revision: String,
    },
}

pub fn custom_agent_revision(config: &CustomAgentConfig) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"custom-agent-config-v1\0");
    hasher.update(serde_json::to_vec(config).expect("agent config serializes"));
    format!("sha256:{:x}", hasher.finalize())
}

pub fn valid_custom_agent_config(config: &CustomAgentConfig) -> bool {
    valid_name(&config.name)
        && config.description.chars().count() <= 4096
        && valid_text(&config.description, true)
        && !config.prompt.trim().is_empty()
        && config.prompt.len() <= 256 * 1024
        && valid_text(&config.prompt, true)
        && valid_references(&config.skills, false)
        && valid_references(&config.mcp, true)
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 64
        && name.trim() == name
        && name.nfc().eq(name.chars())
        && valid_text(name, false)
}

fn valid_text(value: &str, multiline: bool) -> bool {
    value.chars().all(|ch| {
        let code = ch as u32;
        (code >= 32 || multiline && matches!(ch, '\t' | '\n' | '\r'))
            && !(127..=159).contains(&code)
    })
}

fn valid_plugin_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn valid_references(references: &[AgentCapabilityReference], mcp: bool) -> bool {
    if references.len() > 256 {
        return false;
    }
    let mut names = BTreeSet::new();
    references.iter().all(|reference| {
        reference.name.len() <= 64
            && names.insert(&reference.name)
            && if mcp {
                McpServerKey::new(reference.name.clone()).is_ok()
            } else {
                SkillName::parse(reference.name.clone()).is_ok()
            }
            && reference.plugin.as_deref().is_none_or(valid_plugin_name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_validation_matches_center_name_and_reference_rules() {
        let config = CustomAgentConfig {
            name: "库房 CAD 绘图助手".into(),
            description: "完整配置".into(),
            prompt: "请核对图纸。".into(),
            skills: vec![AgentCapabilityReference {
                name: "compact-shelving-cad".into(),
                plugin: Some("warehouse-cad-guidance".into()),
            }],
            mcp: vec![AgentCapabilityReference {
                name: "autocad-ai".into(),
                plugin: None,
            }],
        };
        assert!(valid_custom_agent_config(&config));
        let mut non_nfc = config.clone();
        non_nfc.name = "Cafe\u{301}".into();
        assert!(!valid_custom_agent_config(&non_nfc));
        let mut duplicate = config.clone();
        duplicate.skills.push(config.skills[0].clone());
        assert!(!valid_custom_agent_config(&duplicate));
        assert_eq!(
            custom_agent_revision(&config),
            custom_agent_revision(&config)
        );
    }
}
