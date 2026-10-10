//! Desktop 安装的本机交接，复用现有生命周期锁、实例证明和退出确认。
use super::*;

pub(crate) struct DesktopHandoff {
    _guard: tokio::sync::OwnedMutexGuard<()>,
    coordinator: RuntimeBootstrapCoordinator,
    previous: Option<(RuntimeBootstrap, RuntimeDiscovery)>,
}

impl RuntimeBootstrapCoordinator {
    /// 登录和软件准入之前，仅读取当前已运行进程；不启动、停止或读用户数据。
    pub(crate) async fn desktop_update_target(
        &self,
    ) -> Result<RuntimeBootstrap, RuntimeBootstrapError> {
        self.inspect_target(false)
            .await
            .map(|(bootstrap, _)| bootstrap)
    }

    pub(crate) async fn desktop_handoff(
        &self,
        expected: Option<&RuntimeBootstrap>,
    ) -> Result<DesktopHandoff, RuntimeBootstrapError> {
        let guard = self.lifecycle_gate.clone().lock_owned().await;
        let previous = if let Some(expected) = expected {
            let (actual, discovery) = self.inspect_target(false).await?;
            if actual.instance_id != expected.instance_id
                || actual.base_url != expected.base_url
                || actual.access_token != expected.access_token
            {
                return Err(instance_changed());
            }
            Some((actual, discovery))
        } else {
            None
        };
        Ok(DesktopHandoff {
            _guard: guard,
            coordinator: self.clone(),
            previous,
        })
    }

    pub(crate) async fn resume_desktop_update(
        &self,
        home: &Path,
    ) -> Result<(), RuntimeBootstrapError> {
        let _guard = self.lifecycle_gate.lock().await;
        let home = crate::runtime_source::canonical_home(home).ok_or_else(invalid_discovery)?;
        let coordinator = Self {
            runtime_home: home,
            ..self.clone()
        };
        let executable = coordinator
            .runtime_executable
            .canonicalize()
            .map_err(|_| source_unavailable())?;
        match coordinator.inspect_target(false).await {
            Ok((bootstrap, discovery)) => {
                if discovery.executable_path.as_ref() != Some(&executable)
                    || bootstrap.capabilities.runtime_version
                        != assistant_protocol::SOFTWARE_VERSION
                {
                    return Err(instance_changed());
                }
                coordinator.discover(false).await?;
            }
            Err(_) => {
                if !instance_lock_released(&coordinator.runtime_home)? {
                    return Err(instance_changed());
                }
                coordinator.launch_from(&executable).await?;
                let bootstrap = coordinator.wait_for_start().await?;
                let discovery = read_discovery(&coordinator.runtime_home)?;
                if discovery.executable_path.as_ref() != Some(&executable)
                    || bootstrap.capabilities.runtime_version
                        != assistant_protocol::SOFTWARE_VERSION
                {
                    return Err(instance_changed());
                }
            }
        }
        Ok(())
    }
}

impl DesktopHandoff {
    pub(crate) fn runtime_home(&self) -> Option<PathBuf> {
        self.previous
            .as_ref()
            .map(|_| self.coordinator.runtime_home.clone())
    }

    pub(crate) async fn stop(&self) -> Result<(), RuntimeBootstrapError> {
        let Some((previous, original)) = &self.previous else {
            return Ok(());
        };
        let current = read_discovery(&self.coordinator.runtime_home)?;
        if current.instance_id != original.instance_id
            || current.pid != original.pid
            || current.access_token != original.access_token
            || current.address != original.address
        {
            return Err(instance_changed());
        }
        self.coordinator.stop_for_upgrade(previous).await?;
        self.coordinator.wait_for_instance_to_stop(original).await
    }
}
