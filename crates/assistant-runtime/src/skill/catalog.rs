//! 当前技能扫描结果、结构校验与模型目录渲染。

use agent_model::SystemPromptSnapshot;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    SkillDiagnostic, SkillDiagnosticCode, SkillDiscovery, SkillMetadata, SkillName,
    SkillPackageSourceError, SkillSource,
};

/// 一次扫描的可用性。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillCatalogStatus {
    Ready,
    Empty,
    Unavailable,
    LegacyUnavailable,
}

/// 一次扫描得到的完整 Skill 定义。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkillDefinition {
    pub name: SkillName,
    pub description: String,
    pub source: SkillSource,
    /// 共享 Root 中的源目录；只供 Runtime/工具按需读取，不进入模型目录或 revision。
    pub source_path: String,
    pub definition_digest: String,
    /// 本次扫描读取的正文；接纳输入或执行边界内使用，只有激活正文进入历史消息。
    pub body: String,
    pub metadata: SkillMetadata,
    pub model_invocable: bool,
    pub user_invocable: bool,
}

/// 短生命周期的技能扫描结果，不进入 Session 或 Store。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkillCatalog {
    pub schema_version: u32,
    pub revision: String,
    pub status: SkillCatalogStatus,
    pub definitions: Vec<SkillDefinition>,
    pub diagnostics: Vec<SkillDiagnostic>,
}

impl SkillCatalog {
    pub const SCHEMA_VERSION: u32 = 1;

    /// 旧 Session 的稳定缺省；恢复时不得扫描当前文件补写历史事实。
    pub fn legacy_unavailable() -> Self {
        Self::without_definitions(
            SkillCatalogStatus::LegacyUnavailable,
            vec![SkillDiagnostic::warning(
                SkillDiagnosticCode::LegacyCatalogUnavailable,
                "legacy session does not contain a frozen skill catalog",
            )],
        )
    }

    /// 构造扫描整体不可用的空 Catalog。
    pub fn unavailable(diagnostics: Vec<SkillDiagnostic>) -> Self {
        Self::without_definitions(SkillCatalogStatus::Unavailable, diagnostics)
    }

    /// 从确定性发现结果构造完整 Catalog；Skill 包仍由共享 Root 持有。
    pub fn from_discovery(discovery: SkillDiscovery) -> Result<Self, SkillPackageSourceError> {
        let mut definitions = Vec::with_capacity(discovery.winners.len());
        for winner in discovery.winners {
            definitions.push(SkillDefinition {
                name: winner.name,
                description: winner.description,
                source: winner.source,
                source_path: winner.source_path,
                definition_digest: winner.definition_digest,
                body: winner.body,
                metadata: winner.metadata,
                model_invocable: winner.model_invocable,
                user_invocable: winner.user_invocable,
            });
        }
        definitions.sort_by(|left, right| left.name.cmp(&right.name));
        let status = if definitions.is_empty() {
            SkillCatalogStatus::Empty
        } else {
            SkillCatalogStatus::Ready
        };
        let revision = catalog_revision(&definitions)?;
        let catalog = Self {
            schema_version: Self::SCHEMA_VERSION,
            revision,
            status,
            definitions,
            diagnostics: discovery.diagnostics,
        };
        catalog.validate_structure()?;
        Ok(catalog)
    }

    /// 只从本次扫描目录 解析一项用户可调用定义。
    pub fn user_definition(
        &self,
        name: &SkillName,
    ) -> Result<&SkillDefinition, SkillActivationResolveError> {
        if self.status != SkillCatalogStatus::Ready {
            return Err(SkillActivationResolveError::CatalogUnavailable);
        }
        let definition = self
            .definitions
            .binary_search_by(|definition| definition.name.cmp(name))
            .ok()
            .and_then(|index| self.definitions.get(index))
            .ok_or(SkillActivationResolveError::NotFound)?;
        if !definition.user_invocable {
            return Err(SkillActivationResolveError::NotUserInvocable);
        }
        Ok(definition)
    }

    /// 只从本次扫描目录 解析一项模型可调用定义。
    pub(crate) fn model_definition(
        &self,
        name: &SkillName,
    ) -> Result<&SkillDefinition, ModelSkillResolveError> {
        if matches!(
            self.status,
            SkillCatalogStatus::Unavailable | SkillCatalogStatus::LegacyUnavailable
        ) {
            return Err(ModelSkillResolveError::CatalogUnavailable);
        }
        let definition = self
            .definitions
            .binary_search_by(|definition| definition.name.cmp(name))
            .ok()
            .and_then(|index| self.definitions.get(index))
            .ok_or(ModelSkillResolveError::NotFound)?;
        if !definition.model_invocable {
            return Err(ModelSkillResolveError::NotModelInvocable);
        }
        Ok(definition)
    }

    /// 渲染模型可见的稳定目录 Part；不泄露路径、开关或本机诊断。
    pub fn render_system_prompt_part(&self) -> String {
        let mut output = format!(
            "SKILL_CATALOG_V1\n<available-skills revision=\"{}\">",
            escape_xml(&self.revision)
        );
        for definition in self
            .definitions
            .iter()
            .filter(|definition| definition.model_invocable)
        {
            output.push_str(&format!(
                "\n  <skill name=\"{}\" description=\"{}\" />",
                escape_xml(definition.name.as_str()),
                escape_xml(&definition.description)
            ));
        }
        output.push_str(
            "\n</available-skills>\n<instruction>Use load_skill with an exact name before relying on a skill.</instruction>",
        );
        output
    }

    /// 把 Catalog Part 插入最后一个目录环境 Part 之前，保持 Fork 的目录替换约定。
    pub fn augment_system_prompt(&self, prompt: SystemPromptSnapshot) -> SystemPromptSnapshot {
        let mut parts = prompt.into_parts();
        let directory = parts.pop();
        parts.push(self.render_system_prompt_part());
        if let Some(directory) = directory {
            parts.push(directory);
        }
        SystemPromptSnapshot::new(parts)
    }

    /// 校验反序列化或跨层传入的 Catalog 是否仍满足冻结结构与 revision 不变量。
    pub fn validate_structure(&self) -> Result<(), SkillPackageSourceError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(SkillPackageSourceError::new(
                "skill catalog schema version is unsupported",
            ));
        }
        let ready = self.status == SkillCatalogStatus::Ready;
        if ready != !self.definitions.is_empty()
            || self
                .definitions
                .windows(2)
                .any(|pair| pair[0].name >= pair[1].name)
            || self.definitions.iter().any(|definition| {
                definition.description.trim().is_empty()
                    || definition.source_path.is_empty()
                    || !is_sha256_v1(&definition.definition_digest)
            })
        {
            return Err(SkillPackageSourceError::new(
                "skill catalog structure is invalid",
            ));
        }
        if catalog_revision(&self.definitions)? != self.revision {
            return Err(SkillPackageSourceError::new(
                "skill catalog revision does not match its definitions",
            ));
        }
        Ok(())
    }

    fn without_definitions(status: SkillCatalogStatus, diagnostics: Vec<SkillDiagnostic>) -> Self {
        let definitions = Vec::new();
        Self {
            schema_version: Self::SCHEMA_VERSION,
            revision: catalog_revision(&definitions)
                .expect("serializing an empty stable catalog cannot fail"),
            status,
            definitions,
            diagnostics,
        }
    }
}

/// 用户显式激活无法从当前技能目录 解析的稳定原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkillActivationResolveError {
    CatalogUnavailable,
    NotFound,
    NotUserInvocable,
}

/// 模型 `load_skill` 无法从当前执行目录 解析定义的稳定原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModelSkillResolveError {
    CatalogUnavailable,
    NotFound,
    NotModelInvocable,
}

#[derive(Serialize)]
struct CatalogRevisionEntry<'a> {
    name: &'a SkillName,
    description: &'a str,
    source: SkillSource,
    definition_digest: &'a str,
    model_invocable: bool,
    user_invocable: bool,
}

fn catalog_revision(definitions: &[SkillDefinition]) -> Result<String, SkillPackageSourceError> {
    let entries = definitions
        .iter()
        .map(|definition| CatalogRevisionEntry {
            name: &definition.name,
            description: &definition.description,
            source: definition.source,
            definition_digest: &definition.definition_digest,
            model_invocable: definition.model_invocable,
            user_invocable: definition.user_invocable,
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&entries).map_err(|source| {
        SkillPackageSourceError::with_source("skill catalog revision could not be encoded", source)
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"skill-catalog-v1\0");
    hasher.update(bytes);
    Ok(format!("sha256-v1:{:x}", hasher.finalize()))
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(super) fn is_sha256_v1(value: &str) -> bool {
    value.strip_prefix("sha256-v1:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}
