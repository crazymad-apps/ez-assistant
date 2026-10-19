//! 设备凭据仅用于登记接口；复用现有有界 HTTP 适配，不影响用户登录有效性。
use super::{CenterClient, CenterError, check_protocol_range, decode, valid_center_id};
use assistant_protocol::{HostRegistrationDevice, SecretValue};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct RegistrationReply {
    pub(crate) center_id: String,
    pub(crate) device: Option<HostRegistrationDevice>,
}
impl CenterClient {
    /// 公开发现不携带任何已有凭据；改变地址和重连前均核对中心身份。
    pub(crate) async fn center_identity(&self) -> Result<String, CenterError> {
        #[derive(Deserialize)]
        struct Info {
            center_id: Option<String>,
            protocol_version: u32,
            min_protocol_version: u32,
        }
        let info: Info = decode(self.client.get(self.endpoint("info"))).await?;
        check_protocol_range(info.min_protocol_version, info.protocol_version)?;
        info.center_id
            .filter(|id| valid_center_id(id))
            .ok_or(CenterError::ProtocolIncompatible)
    }
    pub(crate) async fn verify_center(&self, expected: &str) -> Result<(), CenterError> {
        if self.center_identity().await? != expected {
            return Err(CenterError::IdentityMismatch);
        }
        Ok(())
    }
    pub(crate) async fn registration_capability(&self) -> Result<(), CenterError> {
        #[derive(Deserialize)]
        struct Info {
            protocol_version: u32,
            min_protocol_version: u32,
            capabilities: Vec<String>,
        }
        let info: Info = decode(self.client.get(self.endpoint("info"))).await?;
        check_protocol_range(info.min_protocol_version, info.protocol_version)?;
        if !info.capabilities.iter().any(|c| c == "host_registration") {
            return Err(CenterError::ProtocolIncompatible);
        }
        Ok(())
    }
    pub(crate) async fn apply_registration(
        &self,
        token: &SecretValue,
        input: &serde_json::Value,
    ) -> Result<RegistrationReply, CenterError> {
        decode(
            self.client
                .post(self.endpoint("host-devices"))
                .bearer_auth(token.expose())
                .json(input),
        )
        .await
    }
    pub(crate) async fn registration(
        &self,
        center_id: &str,
        id: &str,
        secret: &SecretValue,
        action: Option<&str>,
    ) -> Result<RegistrationReply, CenterError> {
        self.verify_center(center_id).await?;
        let url = self.endpoint(&format!("host-devices/{id}/registration"));
        let request = match action {
            Some(action) => self
                .client
                .post(url)
                .json(&serde_json::json!({"action":action})),
            None => self.client.get(url),
        };
        decode(request.bearer_auth(secret.expose())).await
    }
}
