//! 正式 Host 的 Provider、Session 环境与按 Run 工具装配。

mod environment;
mod model;
mod task_workspace;
mod tools;

use std::{path::Path, sync::Arc};

use assistant_runtime::{
    ChildTaskWorkspaceFactory, ModelServiceFactory, RunToolFactory, SessionEnvironmentFactory,
};

use self::environment::HostSessionEnvironmentFactory;
use self::model::HostModelServiceFactory;
use self::task_workspace::HostChildTaskWorkspaceFactory;
use self::tools::{HostRunToolFactory, ToolResourceError};

pub(crate) struct HostResources {
    pub(crate) model_factory: Arc<dyn ModelServiceFactory>,
    pub(crate) session_environment_factory: Arc<dyn SessionEnvironmentFactory>,
    pub(crate) run_tool_factory: Arc<dyn RunToolFactory>,
    pub(crate) child_task_workspace_factory: Arc<dyn ChildTaskWorkspaceFactory>,
}

impl HostResources {
    pub(crate) fn new(runtime_home: &Path) -> Result<Self, ToolResourceError> {
        Ok(Self {
            model_factory: Arc::new(HostModelServiceFactory),
            session_environment_factory: Arc::new(HostSessionEnvironmentFactory::new(runtime_home)),
            run_tool_factory: Arc::new(HostRunToolFactory::new(runtime_home)?),
            child_task_workspace_factory: Arc::new(HostChildTaskWorkspaceFactory),
        })
    }
}
