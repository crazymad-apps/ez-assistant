//! Shell 默认与 Session 初始化；平台探测归宿主，持久绑定归 Runtime。

use assistant_protocol::ShellKind;

use super::AssistantRuntime;
use crate::{FrozenShellEnvironment, RuntimeError, RuntimeResult};

impl AssistantRuntime {
    /// 查询设置时重新探测目录，客户端不缓存解释器可用性的权威事实。
    pub async fn get_agent_shell_settings(
        &self,
    ) -> RuntimeResult<assistant_protocol::AgentShellSettings> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let catalog = self.agent_shell_catalog().await?;
        let default_agent_shell = self
            .store
            .load_default_agent_shell()
            .await
            .map_err(|source| RuntimeError::from_store("load default agent shell", source))?;
        Ok(assistant_protocol::AgentShellSettings {
            default_agent_shell,
            catalog,
        })
    }

    /// 先核验目标再保存；与 Session 创建使用既有操作门禁串行，不热更新 Session 绑定。
    pub async fn set_default_agent_shell(
        &self,
        shell: ShellKind,
    ) -> RuntimeResult<assistant_protocol::AgentShellSettings> {
        let _operation = self.operation_gate.write().await;
        self.ensure_running()?;
        let frozen = self.freeze_agent_shell(Some(shell)).await?;
        if frozen.as_ref().map(|frozen| frozen.kind) != Some(shell) {
            return Err(RuntimeError::InvalidRequest {
                reason: "requested shell is unavailable",
            });
        }
        let catalog = self.agent_shell_catalog().await?;
        self.store
            .save_default_agent_shell(shell)
            .await
            .map_err(|source| RuntimeError::from_store("save default agent shell", source))?;
        Ok(assistant_protocol::AgentShellSettings {
            default_agent_shell: Some(shell),
            catalog,
        })
    }

    async fn agent_shell_catalog(
        &self,
    ) -> RuntimeResult<Vec<assistant_protocol::ShellCatalogEntry>> {
        let factory = self.run_tool_factory.clone();
        tokio::task::spawn_blocking(move || factory.shell_catalog())
            .await
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "shell catalog task",
            })
    }

    /// 读取创建时的全局默认，并在阻塞池中核验实际解释器；不修改任何既有 Session。
    pub(super) async fn initial_agent_shell(
        &self,
    ) -> RuntimeResult<(Option<ShellKind>, Option<FrozenShellEnvironment>)> {
        let selected = self
            .store
            .load_default_agent_shell()
            .await
            .map_err(|source| RuntimeError::from_store("load default agent shell", source))?;
        let frozen = self.freeze_agent_shell(selected).await?;
        // Unix 未显式设置时继续保持既有的无绑定 /bin/sh 行为。
        let kind = frozen.as_ref().and_then(|shell| {
            (selected.is_some() || shell.kind != ShellKind::PosixSh).then_some(shell.kind)
        });
        Ok((kind, frozen))
    }

    /// 所有注册表与 PATH 探测均发生在阻塞池；失败不得降级为另一解释器。
    pub(super) async fn freeze_agent_shell(
        &self,
        kind: Option<ShellKind>,
    ) -> RuntimeResult<Option<FrozenShellEnvironment>> {
        let factory = self.run_tool_factory.clone();
        tokio::task::spawn_blocking(move || factory.freeze_shell(kind))
            .await
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "shell discovery task",
            })?
            .map_err(|source| RuntimeError::RunToolsBuildFailed { source })
    }
}
