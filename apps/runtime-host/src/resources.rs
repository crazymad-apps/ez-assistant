//! 正式 Host 的 Provider、Session 环境与按 Run 工具装配。

mod environment;
mod model;
mod skills;
mod task_workspace;
mod tools;

use std::{path::Path, sync::Arc};

use assistant_runtime::{
    ChildTaskWorkspaceFactory, ModelServiceFactory, RunToolFactory, SessionEnvironmentFactory,
    SkillPackageSource,
};

use self::environment::HostSessionEnvironmentFactory;
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
    /// M2 建立的四 Root 扫描适配器；M3 在 Session 创建边界注入 Runtime。
    pub(crate) skill_package_source: Arc<dyn SkillPackageSource>,
}

impl HostResources {
    pub(crate) fn new(runtime_home: &Path) -> Result<Self, ToolResourceError> {
        Ok(Self {
            model_factory: Arc::new(HostModelServiceFactory::new(runtime_home)),
            session_environment_factory: Arc::new(HostSessionEnvironmentFactory::new(runtime_home)),
            run_tool_factory: Arc::new(HostRunToolFactory::new(runtime_home)?),
            child_task_workspace_factory: Arc::new(HostChildTaskWorkspaceFactory),
            skill_package_source: Arc::new(HostSkillPackageSource::new(dirs::home_dir())),
        })
    }
}
