//! 原生窗口探针的受控身份夹具：不连接产品 Host，不向外部页面开放 invoke。
//! Profile 选择、窗口归属与清理仍直接执行产品 browser_resource 代码。
use assistant_protocol::{HostIdentityKind, HostLoginResult, HostMode, HostUserIdentity};
use std::sync::Arc;
use tauri::ipc::{CommandArg, CommandItem, InvokeError};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(crate) struct RuntimeTarget {
    identity: Arc<HostLoginResult>,
    origin: String,
    pub(crate) cancellation: CancellationToken,
}
impl RuntimeTarget {
    pub(crate) fn fixture(origin: &str, user: Option<i32>) -> Self {
        Self {
            identity: Arc::new(HostLoginResult {
                token: None,
                expires_at_ms: u64::MAX,
                instance_id: "native-probe".into(),
                mode: if user.is_some() {
                    HostMode::Enterprise
                } else {
                    HostMode::Personal
                },
                kind: HostIdentityKind::User,
                login_context: None,
                identity: user.map(|id| HostUserIdentity {
                    center_id: "native-probe".into(),
                    user_id: id,
                    username: format!("user{id}"),
                    display_name: format!("User {id}"),
                }),
            }),
            origin: origin.into(),
            cancellation: CancellationToken::new(),
        }
    }
    pub(crate) fn same_connection(&self, other: &Self) -> bool {
        self.ensure_active().is_ok()
            && other.ensure_active().is_ok()
            && Arc::ptr_eq(&self.identity, &other.identity)
    }
    pub(crate) fn ensure_active(&self) -> Result<(), String> {
        if self.cancellation.is_cancelled() {
            Err("probe owner changed".into())
        } else {
            Ok(())
        }
    }
    pub(crate) async fn browser_identity(&self) -> Result<(HostLoginResult, bool, String), String> {
        self.ensure_active()?;
        Ok(((*self.identity).clone(), true, self.origin.clone()))
    }
}
impl<'a, R: tauri::Runtime> CommandArg<'a, R> for RuntimeTarget {
    fn from_command(_: CommandItem<'a, R>) -> Result<Self, InvokeError> {
        Err("probe does not accept IPC".into())
    }
}
