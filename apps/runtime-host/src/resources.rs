//! 正式 Host 的 Provider、Session 环境与按 Run 工具装配。

mod environment;
mod model;
mod tools;

use std::{path::Path, sync::Arc};

use assistant_runtime::{ModelServiceFactory, RunToolFactory, SessionEnvironmentFactory};

use self::environment::HostSessionEnvironmentFactory;
use self::model::HostModelServiceFactory;
use self::tools::{HostRunToolFactory, LocalToolMode, ToolResourceError};

pub(crate) struct HostResources {
    pub(crate) model_factory: Arc<dyn ModelServiceFactory>,
    pub(crate) session_environment_factory: Arc<dyn SessionEnvironmentFactory>,
    pub(crate) run_tool_factory: Arc<dyn RunToolFactory>,
}

impl HostResources {
    pub(crate) fn new(
        runtime_home: &Path,
        unsafe_unrestricted_local_tools: bool,
    ) -> Result<Self, ToolResourceError> {
        let mode = if unsafe_unrestricted_local_tools {
            LocalToolMode::UnsafeUnrestricted
        } else {
            LocalToolMode::Safe
        };
        Ok(Self {
            model_factory: Arc::new(HostModelServiceFactory),
            session_environment_factory: Arc::new(HostSessionEnvironmentFactory::new(runtime_home)),
            run_tool_factory: Arc::new(HostRunToolFactory::new(mode, runtime_home)?),
        })
    }
}
