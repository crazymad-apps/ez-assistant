//! 每个企业用户域监督一次当前登录的目录同步；安装与切换仍由 Runtime 拥有。

#[cfg(test)]
mod tests;

use std::sync::{Arc, Weak};

use assistant_protocol::{
    BeginPluginInstallRequest, IdempotencyKey, InstalledPluginSnapshot, PluginOperationStatus,
    PluginPackageOrigin, PluginPackageSelection,
};
use assistant_runtime::{AssistantRuntime, RuntimeError, RuntimeResult};
use tokio_util::sync::CancellationToken;

use crate::access::{
    Credentials,
    enterprise::{CenterLogin, UserKey},
};

pub(super) async fn run(
    runtime: Arc<AssistantRuntime>,
    credentials: Arc<Credentials>,
    key: UserKey,
    cancel: CancellationToken,
) -> Result<(), std::io::Error> {
    let Ok((access, domain)) = credentials.domain_access(&key) else {
        return Ok(());
    };
    let weak_domain = Arc::downgrade(&domain);
    drop(domain);
    let domain = weak_domain;
    let mut changes = credentials.subscribe_changes();
    let mut previous = None;
    loop {
        changes.borrow_and_update();
        let current = domain
            .upgrade()
            .and_then(|domain| credentials.model_login(&key, &domain).ok());
        let Some(login) = current else {
            return Ok(());
        };
        if !previous
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, &login))
        {
            let sync = LoginSync {
                runtime: &runtime,
                credentials: &credentials,
                key: &key,
                domain: &domain,
                login: &login,
            };
            // 只取消读取和等待；已接纳的安装任务由 Runtime 继续监督及关闭。
            sync.refresh(&cancel, &access).await;
            previous = Some(login);
            continue;
        }
        tokio::select! {
            () = cancel.cancelled() => return Ok(()),
            () = access.cancelled() => return Ok(()),
            result = changes.changed() => if result.is_err() { return Ok(()); },
        }
    }
}

struct LoginSync<'a> {
    runtime: &'a AssistantRuntime,
    credentials: &'a Credentials,
    key: &'a UserKey,
    domain: &'a Weak<()>,
    login: &'a Arc<CenterLogin>,
}

impl LoginSync<'_> {
    fn current(&self) -> bool {
        self.domain
            .upgrade()
            .and_then(|domain| self.credentials.model_login(self.key, &domain).ok())
            .is_some_and(|login| Arc::ptr_eq(&login, self.login))
    }

    async fn refresh(&self, cancel: &CancellationToken, access: &CancellationToken) {
        let read = async {
            match self.runtime.list_custom_agents().await {
                Ok(agents) if !agents.center_unavailable => {}
                _ => eprintln!("runtime-host: enterprise login agent refresh unavailable"),
            }
            self.runtime.list_plugins().await
        };
        let installed = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            () = access.cancelled() => return,
            result = read => result,
        };
        let Ok(installed) = installed else {
            eprintln!("runtime-host: enterprise login plugin inventory unavailable");
            return;
        };
        for plugin in installed.plugins {
            if plugin.origin
                != (PluginPackageOrigin::Center {
                    center_id: self.key.center_id.clone(),
                })
            {
                continue;
            }
            let selection = tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                () = access.cancelled() => return,
                result = self.resolve(&plugin) => result,
            };
            let Some(selection) = selection else {
                continue;
            };
            let mut random = [0_u8; 16];
            if getrandom::fill(&mut random).is_err() {
                return;
            }
            let key =
                IdempotencyKey::new(format!("login-plugin-{:032x}", u128::from_be_bytes(random)))
                    .expect("bounded nonempty operation key");
            loop {
                let ready = tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    () = access.cancelled() => return,
                    ready = self.wait_idle() => ready,
                };
                if ready.is_err()
                    || !self.current()
                    || cancel.is_cancelled()
                    || access.is_cancelled()
                {
                    if ready.is_err() && self.current() {
                        eprintln!("runtime-host: enterprise login plugin updates blocked");
                    }
                    return;
                }
                // 不在接纳的持久化/任务登记过程中丢弃 future。
                match self
                    .runtime
                    .begin_plugin_install(BeginPluginInstallRequest {
                        key: key.clone(),
                        packages: vec![selection.clone()],
                    })
                    .await
                {
                    Ok(_) => break,
                    Err(RuntimeError::PluginBusy) => {
                        tokio::select! {
                            () = cancel.cancelled() => return,
                            () = access.cancelled() => return,
                            () = tokio::time::sleep(std::time::Duration::from_millis(250)) => {},
                        }
                    }
                    Err(_) => {
                        eprintln!("runtime-host: enterprise login plugin update not accepted");
                        break;
                    }
                }
            }
        }
    }

    async fn resolve(&self, plugin: &InstalledPluginSnapshot) -> Option<PluginPackageSelection> {
        if !self.current() {
            return None;
        }
        let (center, token) = self.credentials.current_plugin_center(self.key).ok()?;
        let selection = match center
            .resolve_plugin(
                &token,
                &self.key.center_id,
                &plugin.name,
                plugin.sha256.clone(),
            )
            .await
        {
            Ok(selection) => selection,
            Err(_) => {
                eprintln!("runtime-host: enterprise login plugin resolution unavailable");
                return None;
            }
        };
        (self.current() && newer(plugin, &selection)).then_some(selection)
    }

    async fn wait_idle(&self) -> RuntimeResult<()> {
        let mut events = self.runtime.subscribe_events();
        loop {
            if !self.current() {
                return Err(RuntimeError::ConfigurationUnavailable);
            }
            let plugins = self.runtime.list_plugins().await?;
            if plugins.cleanup_blocked
                || plugins
                    .operations
                    .iter()
                    .any(|operation| operation.cleanup_required)
            {
                return Err(RuntimeError::PluginBusy);
            }
            if plugins.operations.iter().all(|operation| {
                matches!(
                    operation.phase,
                    PluginOperationStatus::Succeeded
                        | PluginOperationStatus::Failed
                        | PluginOperationStatus::Cancelled
                        | PluginOperationStatus::Interrupted
                )
            }) {
                // 终态事件可能先于安装任务释放准入锁；下一次接纳仍会重新核验。
                tokio::task::yield_now().await;
                return Ok(());
            }
            tokio::select! {
                _ = events.recv() => {},
                () = tokio::time::sleep(std::time::Duration::from_millis(250)) => {},
            }
        }
    }
}

fn newer(installed: &InstalledPluginSnapshot, selection: &PluginPackageSelection) -> bool {
    let Some((version, sha)) = installed.version.as_ref().zip(installed.sha256.as_ref()) else {
        return false;
    };
    let (Ok(old), Ok(new)) = (
        semver::Version::parse(version),
        semver::Version::parse(&selection.version),
    ) else {
        return false;
    };
    new > old && sha != &selection.sha256
}
