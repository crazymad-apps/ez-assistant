//! 本地 Skill 的领域模型、发现、Session Catalog 与激活机制。

mod activation;
mod catalog;
mod discovery;
mod types;

pub(crate) use activation::{
    LoadSkillAuthorizationFacts, LoadSkillTool, SkillActivationLatch, render_model_activation,
    render_user_activation,
};
#[cfg(test)]
use activation::{LoadSkillInput, LoadSkillStatus};
pub use activation::{SkillActivationOwner, SkillActivationTrigger, StoredSkillActivation};
pub(crate) use catalog::ModelSkillResolveError;
pub use catalog::{
    SessionSkillCatalog, SessionSkillDefinition, SkillActivationResolveError, SkillCatalogStatus,
};
pub(crate) use discovery::EmptySkillPackageSource;
pub use discovery::{
    MAX_CATALOG_SKILLS, SkillCandidate, SkillDiagnostic, SkillDiagnosticCode,
    SkillDiagnosticSeverity, SkillDiscovery, SkillDiscoveryStatus, SkillNameState,
    SkillNameStateChange, SkillPackageSource, SkillPackageSourceError, SkillScanFuture,
    SkillScanRequest, SkillScanResult, compile_skill_discovery, explicit_skill_states,
    sort_diagnostics,
};
pub use types::{SkillMetadata, SkillName, SkillNameError, SkillSource};

#[cfg(test)]
mod tests;
