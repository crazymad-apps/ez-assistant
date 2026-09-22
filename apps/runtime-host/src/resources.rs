//! 正式 Host 的 Provider、Session 环境与按 Run 工具装配。

mod environment;
mod filesystem;
pub(crate) mod model;
pub(crate) mod shell;
mod skills;
mod task_workspace;
mod tools;

use std::{path::Path, sync::Arc};

use assistant_runtime::{
    ChildTaskWorkspaceFactory, ModelServiceFactory, RunToolFactory, SessionEnvironmentFactory,
    SkillPackageSource,
};

use self::environment::HostSessionEnvironmentFactory;
#[cfg(test)]
use self::model::HostModelServiceFactory;
use self::skills::HostSkillPackageSource;
use self::task_workspace::HostChildTaskWorkspaceFactory;
use self::tools::{HostRunToolFactory, ToolResourceError};

/// 正式 Host 集中拥有的具体基础设施适配器集合。
pub(crate) struct HostResources {
    pub(crate) model_factory: Arc<dyn ModelServiceFactory>,
    pub(crate) session_environment_factory: Arc<dyn SessionEnvironmentFactory>,
    pub(crate) run_tool_factory: Arc<dyn RunToolFactory>,
    pub(crate) child_task_workspace_factory: Arc<dyn ChildTaskWorkspaceFactory>,
    /// 按当前用户根装配的 Skill 扫描适配器。
    pub(crate) skill_package_source: Arc<dyn SkillPackageSource>,
}

impl HostResources {
    #[cfg(test)]
    pub(crate) fn new(runtime_home: &Path) -> Result<Self, ToolResourceError> {
        Self::for_user(
            runtime_home,
            true,
            Arc::new(HostModelServiceFactory::new(runtime_home)),
            Arc::new(crate::user_paths::UserPaths::new(runtime_home, Vec::new())),
        )
    }

    pub(crate) fn for_user(
        runtime_home: &Path,
        personal: bool,
        model_factory: Arc<dyn ModelServiceFactory>,
        paths: Arc<crate::user_paths::UserPaths>,
    ) -> Result<Self, ToolResourceError> {
        Ok(Self {
            model_factory,
            session_environment_factory: Arc::new(HostSessionEnvironmentFactory::with_paths(
                runtime_home,
                paths.clone(),
            )),
            run_tool_factory: Arc::new(HostRunToolFactory::with_paths(
                runtime_home,
                paths.clone(),
            )?),
            child_task_workspace_factory: Arc::new(HostChildTaskWorkspaceFactory),
            skill_package_source: Arc::new(if personal {
                HostSkillPackageSource::personal(runtime_home, paths.clone())
            } else {
                HostSkillPackageSource::user(runtime_home, paths.clone())
            }),
        })
    }
}
