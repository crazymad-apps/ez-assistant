//! Desktop 安装的本机交接，复用现有生命周期锁、实例证明和退出确认。
use super::*;

pub(crate) struct DesktopHandoff {
    _guard: tokio::sync::OwnedMutexGuard<()>,
    coordinator: RuntimeBootstrapCoordinator,
    previous: Option<RuntimeDiscovery>,
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
            // 来源已在登录前冻结；安装前只复核同一安全发现记录，不再依赖旧 HTTP。
            let discovery = read_discovery(&self.runtime_home)?;
            validate_discovery(&discovery)?;
            if discovery.instance_id != expected.instance_id
                || discovery.address != expected.base_url
                || discovery.access_token != expected.access_token
            {
                return Err(instance_changed());
            }
            Some(discovery)
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
        let candidate = executable.clone();
        let digest = tokio::task::spawn_blocking(move || crate::runtime_source::digest(&candidate))
            .await
            .map_err(|_| source_unavailable())?
            .map_err(|_| source_unavailable())?;
        crate::runtime_source::verify(
            &executable,
            &digest,
            &assistant_protocol::ClientCompatibility::current(),
        )
        .await
        .map_err(|_| source_unavailable())?;
        match coordinator.inspect_target(false).await {
            Ok((bootstrap, discovery)) => {
                if discovery.executable_path.as_ref() != Some(&executable)
                    || discovery.executable_sha256.as_ref() != Some(&digest)
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
                    || discovery.executable_sha256.as_ref() != Some(&digest)
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

    /// 已完成下载与验签后消费交接对象；原生任务持有 gate，取消等待也不会提前放行。
    /// 成功时归还同一对象供安装阶段继续持锁，失败则释放，不启动或修补业务状态。
    pub(crate) async fn stop(self) -> Result<Self, RuntimeBootstrapError> {
        if self.previous.is_none() {
            return Ok(self);
        };
        let (guard, _) = self
            .coordinator
            .stop_native(self._guard, self.previous.clone())
            .await?;
        Ok(Self {
            _guard: guard,
            coordinator: self.coordinator,
            previous: self.previous,
        })
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// 来源绑定后暂停真实 Host；安装停止必须仅依赖原生锁归属，不再请求已失去响应的 HTTP。
    #[tokio::test]
    #[ignore = "需要显式 EZ_ASSISTANT_TEST_HOST_BINARY；只新建项目隔离 Home"]
    async fn isolated_update_handoff_stops_unresponsive_host_and_rejects_changed_instance() {
        let binary = PathBuf::from(std::env::var_os("EZ_ASSISTANT_TEST_HOST_BINARY").unwrap())
            .canonicalize()
            .unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../.runtime-test")
            .canonicalize()
            .unwrap();
        let home = tempfile::Builder::new()
            .prefix("f01-update-handoff-")
            .tempdir_in(root)
            .unwrap();
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        fs::write(home.path().join("host.toml"), format!("version = \"0.27.0\"\nmode = \"enterprise\"\n[enterprise]\ncenter_url = \"http://127.0.0.1:9\"\n[host_access]\nremote_enabled = false\nport = {port}\n")).unwrap();
        fs::set_permissions(
            home.path().join("host.toml"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let mut child = tokio::process::Command::new(&binary)
            .args(["serve", "--runtime-home"])
            .arg(home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let coordinator = RuntimeBootstrapCoordinator::new(home.path().to_owned(), binary).unwrap();
        let worker = coordinator.clone();
        let result = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
            let mut expected = loop {
                if let Ok(value) = worker.desktop_update_target().await {
                    break value;
                }
                assert!(tokio::time::Instant::now() < deadline);
                tokio::time::sleep(POLL_INTERVAL).await;
            };
            let original = read_discovery(&worker.runtime_home).unwrap();
            expected.instance_id = "changed-instance".into();
            assert!(worker.desktop_handoff(Some(&expected)).await.is_err());
            expected.instance_id = original.instance_id;
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(original.pid as i32),
                nix::sys::signal::Signal::SIGSTOP,
            )
            .unwrap();
            let handoff = tokio::time::timeout(
                Duration::from_secs(1),
                worker.desktop_handoff(Some(&expected)),
            )
            .await
            .unwrap()
            .unwrap();
            let handoff = handoff.stop().await.unwrap();
            assert!(instance_lock_released(&worker.runtime_home).unwrap());
            assert!(
                worker.lifecycle_gate.try_lock().is_err(),
                "installation retains the same gate"
            );
            drop(handoff);
            assert!(worker.lifecycle_gate.try_lock().is_ok());
        })
        .await;
        if child.try_wait().unwrap().is_none() {
            child.kill().await.unwrap();
        }
        child.wait().await.unwrap();
        result.unwrap();
    }
}
