//! HostAccess actor 内的登记意图处理；网络等待无锁，提交前保存可恢复的身份材料。
pub(crate) mod network;
mod store;
#[cfg(test)]
mod tests;
use super::{
    AccessError, AccessPermit, HostAccessService,
    enterprise::{CenterClient, RegistrationReply},
};
use assistant_protocol::{
    HostMode, HostRegistrationCommand as Command, HostRegistrationIntent as Intent,
    HostRegistrationStatus as Status,
};
use store::{Enrollment, Identity, Store};

impl HostAccessService {
    pub(super) async fn registration(
        &self,
        command: Command,
        mut permit: AccessPermit,
    ) -> Result<Status, AccessError> {
        permit.check()?;
        if self.identity_restart_required {
            return Err(AccessError::Invalid(
                "中心配置已保存，请重启 Host 后管理设备登记。",
            ));
        }
        let document = super::config::load(self.source.as_ref()).await?;
        let host = crate::host_configuration::parse(&document.document.to_string())
            .map_err(AccessError::Invalid)?;
        let mut store = Store::open(self.source.as_ref()).await?;
        if host.mode != HostMode::Enterprise {
            return if matches!(command, Command::GetStatus) {
                Ok(status(&store, None))
            } else {
                Err(AccessError::Invalid("请先配置企业模式并重启 Host。"))
            };
        }
        let configured = host.enterprise.as_ref().and_then(|e| e.center_id.clone());
        let center_id = configured.as_deref().unwrap_or_default();
        let applying = matches!(command, Command::Apply { .. });
        let client = self
            .center
            .read()
            .map_err(|_| AccessError::Unavailable)?
            .clone();
        if let Command::Apply { ref name } = command {
            if name.trim() != name
                || name.is_empty()
                || name.encode_utf16().count() > 80
                || name.chars().any(char::is_control)
            {
                return Err(AccessError::Invalid(
                    "设备名称须为 1—80 个字符且不含控制字符。",
                ));
            }
            self.credentials.verify_enterprise(&mut permit).await?;
            client
                .as_ref()
                .ok_or(AccessError::Unavailable)?
                .registration_capability()
                .await?;
            permit.check()?;
            let new = store
                .identity
                .as_ref()
                .and_then(|i| i.registrations.get(center_id))
                .is_none_or(|e| e.intent == Intent::Closed);
            if new {
                let center_id = configured.clone().ok_or(AccessError::Unavailable)?;
                if store.identity.is_none() {
                    store.identity = Some(Identity {
                        installation_id: store::new_id()?,
                        registrations: Default::default(),
                    });
                }
                let identity = store.identity.as_mut().ok_or(AccessError::Unavailable)?;
                identity.registrations.insert(
                    center_id.clone(),
                    Enrollment {
                        center_id,
                        id: store::new_id()?,
                        secret: store::new_secret()?,
                        name: name.clone(),
                        platform: std::env::consts::OS.to_owned(),
                        host_version: env!("CARGO_PKG_VERSION").to_owned(),
                        intent: Intent::Publish,
                    },
                );
                store.save().await?;
            }
        }
        if matches!(command, Command::Exit | Command::Withdraw)
            && let Some(e) = store
                .identity
                .as_mut()
                .and_then(|i| i.registrations.get_mut(center_id))
            && e.intent != Intent::Closed
        {
            // 退出具有更强意图，后来的撤回不得把待注销降回仅撤回。
            e.intent = if matches!(command, Command::Exit) || e.intent == Intent::Exit {
                Intent::Exit
            } else {
                Intent::Withdraw
            };
            permit.check()?;
            store.save().await?;
            self.publish_network().await?;
        }
        let mut result = status(&store, configured.clone());
        let Some(identity) = store.identity.as_ref() else {
            return Ok(result);
        };
        let Some(e) = identity.registrations.get(center_id) else {
            return Ok(result);
        };
        // 仅当前中心的登记可以发起请求，其他中心留存材料不参与连接。
        if host.mode != HostMode::Enterprise
            || host
                .enterprise
                .as_ref()
                .and_then(|c| c.center_id.as_deref())
                != Some(&e.center_id)
        {
            return Ok(result);
        }
        let client = client.ok_or(AccessError::Unavailable)?;
        let outcome = self
            .synchronize(&client, identity, e, &mut permit, applying)
            .await;
        match outcome {
            Ok(reply) => {
                let terminal = reply
                    .device
                    .as_ref()
                    .is_some_and(|d| d.status.is_terminal());
                result.device = reply.device;
                if terminal && e.intent != Intent::Closed {
                    store
                        .identity
                        .as_mut()
                        .and_then(|i| i.registrations.get_mut(center_id))
                        .ok_or(AccessError::Unavailable)?
                        .intent = Intent::Closed;
                    store.save().await?;
                    result.intent = Some(Intent::Closed);
                }
            }
            Err(error) => {
                result.error = Some(error.protocol_info());
            }
        }
        result.connection = *self.network.status.borrow();
        Ok(result)
    }

    async fn synchronize(
        &self,
        client: &CenterClient,
        identity: &Identity,
        e: &Enrollment,
        permit: &mut AccessPermit,
        applying: bool,
    ) -> Result<RegistrationReply, AccessError> {
        let mut reply = client
            .registration(&e.center_id, &e.id, &e.secret, None)
            .await?;
        check_reply(&reply, identity, e)?;
        if reply.device.is_none()
            && (applying || matches!(e.intent, Intent::Exit | Intent::Withdraw))
        {
            // 结果未知沿用原 ID/秘密及原申请内容；取消也须先确认该原申请，不凭 null 丢弃凭据。
            self.credentials.verify_enterprise(permit).await?;
            let key = permit.user_key().ok_or(AccessError::Unauthorized)?;
            let (current, token, center_id) = self.credentials.current_agent_center(key)?;
            if current.url != client.url || center_id != e.center_id {
                return Err(super::enterprise::CenterError::IdentityMismatch.into());
            }
            permit.check()?;
            reply = client.apply_registration(&token, &serde_json::json!({
                "center_id":e.center_id, "id":e.id, "installation_id":identity.installation_id,
                "secret":e.secret, "name":e.name, "platform":e.platform, "host_version":e.host_version,
            })).await?;
            check_reply(&reply, identity, e)?;
        }
        if matches!(e.intent, Intent::Withdraw | Intent::Exit)
            && reply
                .device
                .as_ref()
                .is_some_and(|d| !d.status.is_terminal())
        {
            reply = client
                .registration(
                    &e.center_id,
                    &e.id,
                    &e.secret,
                    Some(if e.intent == Intent::Exit {
                        "exit"
                    } else {
                        "withdraw"
                    }),
                )
                .await?;
            check_reply(&reply, identity, e)?;
        }
        Ok(reply)
    }
}
fn check_reply(
    reply: &RegistrationReply,
    identity: &Identity,
    e: &Enrollment,
) -> Result<(), AccessError> {
    if reply.center_id != e.center_id
        || reply
            .device
            .as_ref()
            .is_some_and(|d| d.id != e.id || d.installation_id != identity.installation_id)
    {
        return Err(super::enterprise::CenterError::IdentityMismatch.into());
    }
    Ok(())
}
fn status(store: &Store, center_id: Option<String>) -> Status {
    let identity = store.identity.as_ref();
    let enrollment = identity.and_then(|i| i.registrations.get(center_id.as_deref()?));
    Status {
        center_id,
        installation_id: identity.map(|i| i.installation_id.clone()),
        registration_id: enrollment.map(|e| e.id.clone()),
        intent: enrollment.map(|e| e.intent),
        device: None,
        error: None,
        connection: Default::default(),
    }
}

/// 统一设备路由参数只接受已有登记标识形状。
pub(crate) fn valid_device_id(value: &str) -> bool {
    store::valid_id(value)
}
