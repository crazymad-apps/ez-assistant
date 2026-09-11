//! 应用协议按双方软件版本与最低兼容版本准入；不依赖传输、存储或客户端实现。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{MIN_COMPATIBLE_VERSION, SOFTWARE_VERSION, parse_software_version};

pub const CLIENT_VERSION_HEADER: &str = "x-ez-client-version";
pub const MIN_COMPATIBLE_VERSION_HEADER: &str = "x-ez-min-compatible-version";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ClientCompatibility {
    pub version: String,
    pub min_compatible_version: String,
}

impl ClientCompatibility {
    pub fn current() -> Self {
        Self {
            version: SOFTWARE_VERSION.into(),
            min_compatible_version: MIN_COMPATIBLE_VERSION.into(),
        }
    }

    pub fn is_valid(&self) -> bool {
        matches!((parse_software_version(&self.version), parse_software_version(&self.min_compatible_version)),
            (Some(version), Some(minimum)) if minimum <= version)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum RuntimeCompatibilityErrorCode {
    MissingDeclaration,
    InvalidDeclaration,
    ClientTooOld,
    HostTooOld,
}

/// 仅回传已经验证的版本对，非法原始文本不能进入诊断。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RuntimeCompatibilityError {
    pub code: RuntimeCompatibilityErrorCode,
    pub client: Option<ClientCompatibility>,
    pub host: Option<ClientCompatibility>,
}

pub fn check_compatibility(
    client: Option<&ClientCompatibility>,
    host: &ClientCompatibility,
) -> Result<(), RuntimeCompatibilityError> {
    use RuntimeCompatibilityErrorCode::*;
    let code = match client {
        _ if !host.is_valid() => InvalidDeclaration,
        None => MissingDeclaration,
        Some(client) if !client.is_valid() => InvalidDeclaration,
        Some(client)
            if parse_software_version(&client.version)
                < parse_software_version(&host.min_compatible_version) =>
        {
            ClientTooOld
        }
        Some(client)
            if parse_software_version(&host.version)
                < parse_software_version(&client.min_compatible_version) =>
        {
            HostTooOld
        }
        Some(_) => return Ok(()),
    };
    Err(RuntimeCompatibilityError {
        code,
        client: client.filter(|value| value.is_valid()).cloned(),
        host: host.is_valid().then(|| host.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Case {
        name: String,
        client: Option<ClientCompatibility>,
        host: ClientCompatibility,
        error: Option<RuntimeCompatibilityErrorCode>,
    }

    #[test]
    fn shared_software_compatibility_vectors() {
        let cases: Vec<Case> = serde_json::from_str(include_str!(
            "../../../packages/assistant-protocol/fixtures/compatibility.json"
        ))
        .unwrap();
        for case in cases {
            let outcome = check_compatibility(case.client.as_ref(), &case.host);
            assert_eq!(
                outcome.as_ref().err().map(|error| error.code),
                case.error,
                "{}",
                case.name
            );
            if let Err(error) = outcome {
                assert!(
                    error
                        .client
                        .as_ref()
                        .is_none_or(ClientCompatibility::is_valid)
                );
                assert!(
                    error
                        .host
                        .as_ref()
                        .is_none_or(ClientCompatibility::is_valid)
                );
            }
        }
    }
}
