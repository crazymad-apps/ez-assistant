//! 本地 Skill 的领域模型、发现与激活机制。

mod activation;
mod catalog;
mod discovery;
mod reference;
mod types;

pub(crate) use activation::{
    LoadSkillAuthorizationFacts, LoadSkillTool, SkillActivationLatch, StagedSkillDefinition,
    render_model_activation, render_user_activation,
};
#[cfg(test)]
use activation::{LoadSkillInput, LoadSkillStatus};
pub use activation::{SkillActivationOwner, SkillActivationTrigger, StoredSkillActivation};
pub(crate) use catalog::ModelSkillResolveError;
pub use catalog::{SkillActivationResolveError, SkillCatalog, SkillCatalogStatus, SkillDefinition};
pub(crate) use discovery::EmptySkillPackageSource;
pub use discovery::{
    MAX_CATALOG_SKILLS, PluginSkillPackage, SkillCandidate, SkillDiagnostic, SkillDiagnosticCode,
    SkillDiagnosticSeverity, SkillDiscovery, SkillDiscoveryStatus, SkillNameState,
    SkillNameStateChange, SkillPackageSource, SkillPackageSourceError, SkillScanFuture,
    SkillScanRequest, SkillScanResult, compile_skill_discovery, explicit_skill_states,
    sort_diagnostics,
};
pub use discovery::{SkillReferenceFuture, SkillReferenceRequest};
pub use reference::{
    BUILTIN_GUIDE_REFERENCES, BUILTIN_GUIDE_SKILL_NAME, MAX_SKILL_REFERENCE_BYTES,
};
pub(crate) use reference::{ReadSkillReferenceAuthorizationFacts, ReadSkillReferenceTool};
pub use types::{SkillMetadata, SkillName, SkillNameError, SkillSource};

#[cfg(test)]
mod tests;
