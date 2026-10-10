//! 更新清单只验证一次；签名将版本、平台和下载摘要绑定在一起。
use assistant_protocol::{ClientCompatibility, check_compatibility, parse_software_version};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use url::Url;

use super::{UpdateError, UpdateSource};

pub(super) const FILE_LIMIT: u64 = 512 * 1024 * 1024;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    product: String,
    version: String,
    min_supported_version: String,
    target: String,
    host: Host,
    files: Vec<Artifact>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Host {
    version: String,
    min_supported_version: String,
    enterprise_protocols: Vec<u32>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Artifact {
    name: String,
    purposes: Vec<String>,
    pub(super) size: u64,
    pub(super) sha256: String,
    pub(super) signature: Option<String>,
}

#[derive(Deserialize)]
struct Announcement {
    version: String,
    notes: String,
    url: Url,
    signature: String,
    manifest: String,
    manifest_signature: String,
    manifest_sha256: String,
}

#[derive(Clone)]
pub(super) struct VerifiedManifest {
    pub(super) version: String,
    pub(super) notes: String,
    pub(super) url: Url,
    pub(super) manifest_sha256: String,
    pub(super) artifact: Artifact,
}

fn decoded_text(text: &str) -> Result<String, UpdateError> {
    if text.len() > 8192 {
        return Err(UpdateError::InvalidPackage);
    }
    if text.trim().starts_with("untrusted comment:") {
        return Ok(text.trim().to_owned());
    }
    String::from_utf8(
        STANDARD
            .decode(text.trim())
            .map_err(|_| UpdateError::InvalidPackage)?,
    )
    .map_err(|_| UpdateError::InvalidPackage)
}

pub(super) fn public_key(text: &str) -> Result<PublicKey, UpdateError> {
    PublicKey::decode(&decoded_text(text)?).map_err(|_| UpdateError::InvalidPackage)
}

pub(super) fn signature(text: &str) -> Result<Signature, UpdateError> {
    Signature::decode(&decoded_text(text)?).map_err(|_| UpdateError::InvalidPackage)
}

fn valid_version(value: &str) -> bool {
    value.split('.').count() == 3
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 6
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'))
        })
}

fn compatibility(version: &str, minimum: &str) -> Result<ClientCompatibility, UpdateError> {
    let value = ClientCompatibility {
        version: version.into(),
        min_compatible_version: minimum.into(),
    };
    if !valid_version(version) || !valid_version(minimum) || !value.is_valid() {
        return Err(UpdateError::InvalidPackage);
    }
    Ok(value)
}

impl VerifiedManifest {
    pub(super) fn read(
        json: &serde_json::Value,
        key: &PublicKey,
        source: &UpdateSource,
        target: &str,
        current: &str,
        center_protocol: (u32, u32),
    ) -> Result<Self, UpdateError> {
        if serde_json::to_vec(json)
            .map_err(|_| UpdateError::InvalidPackage)?
            .len()
            > 256 * 1024
        {
            return Err(UpdateError::InvalidPackage);
        }
        let announcement: Announcement =
            serde_json::from_value(json.clone()).map_err(|_| UpdateError::InvalidPackage)?;
        if announcement.notes.len() > 16 * 1024 || announcement.manifest.len() > 64 * 1024 {
            return Err(UpdateError::InvalidPackage);
        }
        key.verify(
            announcement.manifest.as_bytes(),
            &signature(&announcement.manifest_signature)?,
            false,
        )
        .map_err(|_| UpdateError::InvalidPackage)?;
        let digest = format!("{:x}", Sha256::digest(announcement.manifest.as_bytes()));
        if digest != announcement.manifest_sha256 {
            return Err(UpdateError::InvalidPackage);
        }
        let manifest: Manifest = serde_json::from_str(&announcement.manifest)
            .map_err(|_| UpdateError::InvalidPackage)?;
        let desktop = compatibility(&manifest.version, &manifest.min_supported_version)?;
        let bundled = compatibility(&manifest.host.version, &manifest.host.min_supported_version)?;
        if manifest.product != "ez-assistant"
            || manifest.target != target
            || manifest.version != announcement.version
            || !valid_version(current)
            || parse_software_version(&manifest.version) <= parse_software_version(current)
            || manifest.host.enterprise_protocols.is_empty()
            || manifest.host.enterprise_protocols.len() > 16
            || manifest
                .host
                .enterprise_protocols
                .iter()
                .any(|p| *p == 0 || *p > 65535)
            || check_compatibility(Some(&desktop), &bundled).is_err()
        {
            return Err(UpdateError::InvalidPackage);
        }
        let host = if source.local.is_some() {
            &bundled
        } else {
            &source.host
        };
        let protocols = if source.local.is_some() {
            &manifest.host.enterprise_protocols
        } else {
            &source.protocols
        };
        if check_compatibility(Some(&desktop), host).is_err()
            || !protocols
                .iter()
                .any(|p| center_protocol.0 <= *p && *p <= center_protocol.1)
        {
            return Err(UpdateError::InvalidPackage);
        }
        if manifest.files.is_empty() || manifest.files.len() > 2 {
            return Err(UpdateError::InvalidPackage);
        }
        for file in &manifest.files {
            if file.name.is_empty()
                || file.name.len() > 181
                || !file.name.as_bytes()[0].is_ascii_alphanumeric()
                || !file
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                || file.size == 0
                || file.size > FILE_LIMIT
                || file.sha256.len() != 64
                || !file
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || file.purposes.is_empty()
                || file.purposes.len() > 2
                || file
                    .purposes
                    .iter()
                    .any(|p| p != "installer" && p != "updater")
            {
                return Err(UpdateError::InvalidPackage);
            }
        }
        let updater: Vec<_> = manifest
            .files
            .iter()
            .filter(|file| file.purposes.iter().any(|p| p == "updater"))
            .collect();
        if updater.len() != 1 {
            return Err(UpdateError::InvalidPackage);
        }
        let artifact = updater[0].clone();
        if artifact.signature.as_deref() != Some(&announcement.signature)
            || !(if target.starts_with("windows-") {
                artifact.name.ends_with(".msi")
            } else {
                artifact.name.ends_with(".app.tar.gz")
            })
            || announcement.url
                != source.endpoint(&format!(
                    "{target}/releases/{}/files/updater",
                    manifest.version
                ))?
        {
            return Err(UpdateError::InvalidPackage);
        }
        // 提前拒绝不能流式验签的制品；下载过程仅做一次签名哈希与 SHA。
        key.verify_stream(&signature(&announcement.signature)?)
            .map_err(|_| UpdateError::InvalidPackage)?;
        Ok(Self {
            version: manifest.version,
            notes: announcement.notes,
            url: announcement.url,
            manifest_sha256: digest,
            artifact,
        })
    }
}
