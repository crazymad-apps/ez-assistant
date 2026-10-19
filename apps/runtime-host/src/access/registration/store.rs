//! 身份材料复用现有私有文件读取与 CAS 原子替换，不另建配置写入实现。
use super::super::AccessError;
use crate::config_source::{LocalConfigSource, prepare_private_directory};
use assistant_protocol::{HostRegistrationIntent, SecretValue};
use assistant_runtime::{ConfigSourceLoad, ConfigSourceReplace, RuntimeConfigSource};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Deserialize, Serialize)]
#[serde(try_from = "StoredIdentity")]
pub(super) struct Identity {
    pub installation_id: String,
    pub registrations: BTreeMap<String, Enrollment>,
}

// 已交付测试包中存在单中心身份文件；只在读取边界归一化，写回只使用按中心索引的格式。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredIdentity {
    installation_id: String,
    #[serde(default)]
    registrations: BTreeMap<String, Enrollment>,
    current: Option<Enrollment>,
}
impl TryFrom<StoredIdentity> for Identity {
    type Error = &'static str;
    fn try_from(mut stored: StoredIdentity) -> Result<Self, Self::Error> {
        if let Some(enrollment) = stored.current {
            if stored.registrations.contains_key(&enrollment.center_id) {
                return Err("设备身份重复，不能自动覆盖。");
            }
            stored
                .registrations
                .insert(enrollment.center_id.clone(), enrollment);
        }
        Ok(Self {
            installation_id: stored.installation_id,
            registrations: stored.registrations,
        })
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Enrollment {
    pub center_id: String,
    pub id: String,
    pub secret: SecretValue,
    pub name: String,
    pub platform: String,
    pub host_version: String,
    pub intent: HostRegistrationIntent,
}
pub(super) struct Store {
    source: LocalConfigSource,
    revision: Option<String>,
    pub identity: Option<Identity>,
}
impl Store {
    pub async fn open(host: &LocalConfigSource) -> Result<Self, AccessError> {
        let parent = host
            .path()
            .parent()
            .ok_or(AccessError::Unavailable)?
            .join("network");
        let check = parent.clone();
        tokio::task::spawn_blocking(move || match std::fs::symlink_metadata(&check) {
            Ok(_) => prepare_private_directory(&check).map_err(|_| AccessError::Unavailable),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(AccessError::Unavailable),
        })
        .await
        .map_err(|_| AccessError::Unavailable)??;
        let source = LocalConfigSource::new(parent.join("identity.json"));
        let (identity, revision) = match source.load().await {
            ConfigSourceLoad::Missing => (None, None),
            ConfigSourceLoad::Document(doc) => {
                let identity: Identity = serde_json::from_str(doc.contents()).map_err(|_| {
                    AccessError::Invalid("设备身份文件损坏，请核实原登记，不能自动覆盖。")
                })?;
                if !valid_id(&identity.installation_id)
                    || identity.registrations.iter().any(|(center, e)| {
                        center != &e.center_id
                            || !valid_id(&e.id)
                            || !valid_id(&e.center_id)
                            || e.secret.expose().len() != 64
                            || !e
                                .secret
                                .expose()
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                            || e.name.is_empty()
                            || e.name.len() > 320
                    })
                {
                    return Err(AccessError::Invalid("设备身份材料无效，请核实原登记。"));
                }
                (Some(identity), Some(doc.revision().to_owned()))
            }
            ConfigSourceLoad::Unavailable(_) => return Err(AccessError::Unavailable),
        };
        Ok(Self {
            source,
            revision,
            identity,
        })
    }
    pub async fn save(&mut self) -> Result<(), AccessError> {
        let parent = self
            .source
            .path()
            .parent()
            .ok_or(AccessError::Unavailable)?
            .to_owned();
        tokio::task::spawn_blocking(move || prepare_private_directory(&parent))
            .await
            .map_err(|_| AccessError::Unavailable)?
            .map_err(|_| AccessError::Unavailable)?;
        let value = serde_json::to_string(self.identity.as_ref().ok_or(AccessError::Unavailable)?)
            .map_err(|_| AccessError::Unavailable)?;
        match self.source.replace(self.revision.clone(), value).await {
            ConfigSourceReplace::Applied(doc) => {
                self.revision = Some(doc.revision().to_owned());
                Ok(())
            }
            ConfigSourceReplace::Conflict(_) => Err(AccessError::Conflict),
            ConfigSourceReplace::Unavailable(_) => Err(AccessError::Unavailable),
        }
    }
}
pub(super) fn valid_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
        && &value[14..15] == "4"
        && matches!(&value[19..20], "8" | "9" | "a" | "b")
}
pub(super) fn new_id() -> Result<String, AccessError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| AccessError::Unavailable)?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let hex = hex(&bytes);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
pub(super) fn new_secret() -> Result<SecretValue, AccessError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| AccessError::Unavailable)?;
    Ok(SecretValue::new(hex(&bytes)))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
