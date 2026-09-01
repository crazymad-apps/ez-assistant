//! Runtime Home 内安装级 TLS 身份的生成、校验与私有发布。

use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use getrandom::fill;
use rcgen::generate_simple_self_signed;
use rustls_pki_types::{CertificateDer, pem::PemObject};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const DEVICE_DIRECTORY: &str = "device";
const INSTALLATION_FILE: &str = "installation.json";
const CERTIFICATE_FILE: &str = "server-cert.pem";
const PRIVATE_KEY_FILE: &str = "server-key.pem";

/// 当前 Runtime Home 的安装级 TLS 身份及其对外可见标识。
///
/// 私钥字节只用于装配 WSS listener，不进入 Desktop 快照或 Runtime 业务状态。
#[derive(Clone)]
pub(super) struct InstallationIdentity {
    pub(super) installation_id: String,
    pub(super) certificate_fingerprint: String,
    pub(super) certificate_pem: Vec<u8>,
    pub(super) private_key_pem: Vec<u8>,
}

/// `installation.json` 的版本化持久格式；证书和私钥分别保存在受限权限文件中。
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstallationDocument {
    schema_version: u32,
    installation_id: String,
    certificate_fingerprint: String,
}

impl InstallationIdentity {
    pub(super) fn load_or_create(runtime_home: &Path) -> Result<Self, IdentityError> {
        let directory = runtime_home.join(DEVICE_DIRECTORY);
        prepare_private_directory(&directory)?;
        let installation_path = directory.join(INSTALLATION_FILE);
        let certificate_path = directory.join(CERTIFICATE_FILE);
        let private_key_path = directory.join(PRIVATE_KEY_FILE);

        if installation_path.exists() || certificate_path.exists() || private_key_path.exists() {
            return Self::load(&installation_path, &certificate_path, &private_key_path);
        }

        Self::create(
            &directory,
            &installation_path,
            &certificate_path,
            &private_key_path,
        )
    }

    fn load(
        installation_path: &Path,
        certificate_path: &Path,
        private_key_path: &Path,
    ) -> Result<Self, IdentityError> {
        ensure_regular_private_file(installation_path)?;
        ensure_regular_private_file(certificate_path)?;
        ensure_regular_private_file(private_key_path)?;
        let document: InstallationDocument =
            serde_json::from_slice(&fs::read(installation_path).map_err(IdentityError::Read)?)
                .map_err(IdentityError::Document)?;
        if document.schema_version != 1 || !valid_installation_id(&document.installation_id) {
            return Err(IdentityError::InvalidMetadata);
        }
        let certificate_pem = fs::read(certificate_path).map_err(IdentityError::Read)?;
        let private_key_pem = fs::read(private_key_path).map_err(IdentityError::Read)?;
        let fingerprint = certificate_fingerprint(&certificate_pem)?;
        if fingerprint != document.certificate_fingerprint {
            return Err(IdentityError::FingerprintMismatch);
        }
        Ok(Self {
            installation_id: document.installation_id,
            certificate_fingerprint: fingerprint,
            certificate_pem,
            private_key_pem,
        })
    }

    fn create(
        directory: &Path,
        installation_path: &Path,
        certificate_path: &Path,
        private_key_path: &Path,
    ) -> Result<Self, IdentityError> {
        let installation_id = random_hex(16)?;
        let subject_name = format!("{installation_id}.ez-assistant.local");
        let certified = generate_simple_self_signed(vec![subject_name, "localhost".to_owned()])
            .map_err(IdentityError::Generate)?;
        let certificate_pem = certified.cert.pem().into_bytes();
        let private_key_pem = certified.signing_key.serialize_pem().into_bytes();
        let certificate_fingerprint = certificate_fingerprint(&certificate_pem)?;
        let document = InstallationDocument {
            schema_version: 1,
            installation_id: installation_id.clone(),
            certificate_fingerprint: certificate_fingerprint.clone(),
        };
        let installation_json =
            serde_json::to_vec_pretty(&document).map_err(IdentityError::Document)?;

        let suffix = random_hex(8)?;
        let temporary_certificate = directory.join(format!(".{CERTIFICATE_FILE}.{suffix}.tmp"));
        let temporary_key = directory.join(format!(".{PRIVATE_KEY_FILE}.{suffix}.tmp"));
        let temporary_installation = directory.join(format!(".{INSTALLATION_FILE}.{suffix}.tmp"));
        write_new_private(&temporary_certificate, &certificate_pem)?;
        write_new_private(&temporary_key, &private_key_pem)?;
        write_new_private(&temporary_installation, &installation_json)?;
        fs::rename(&temporary_certificate, certificate_path).map_err(IdentityError::Publish)?;
        fs::rename(&temporary_key, private_key_path).map_err(IdentityError::Publish)?;
        fs::rename(&temporary_installation, installation_path).map_err(IdentityError::Publish)?;
        sync_directory(directory)?;

        Ok(Self {
            installation_id,
            certificate_fingerprint,
            certificate_pem,
            private_key_pem,
        })
    }
}

fn prepare_private_directory(path: &Path) -> Result<(), IdentityError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(IdentityError::UnsafePath(path.to_path_buf()));
        }
    } else {
        fs::create_dir(path).map_err(IdentityError::CreateDirectory)?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(IdentityError::Permissions)
}

fn ensure_regular_private_file(path: &Path) -> Result<(), IdentityError> {
    let metadata = fs::symlink_metadata(path).map_err(IdentityError::Read)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(IdentityError::UnsafePath(path.to_path_buf()));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(IdentityError::UnsafePermissions(path.to_path_buf()));
    }
    Ok(())
}

fn write_new_private(path: &Path, content: &[u8]) -> Result<(), IdentityError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(IdentityError::Write)?;
    file.write_all(content).map_err(IdentityError::Write)?;
    file.sync_all().map_err(IdentityError::Write)
}

fn sync_directory(path: &Path) -> Result<(), IdentityError> {
    OpenOptions::new()
        .read(true)
        .open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(IdentityError::Publish)
}

fn certificate_fingerprint(certificate_pem: &[u8]) -> Result<String, IdentityError> {
    let certificate = CertificateDer::from_pem_slice(certificate_pem)
        .map_err(|_| IdentityError::InvalidCertificate)?;
    Ok(hex(&Sha256::digest(certificate.as_ref())))
}

fn random_hex(bytes: usize) -> Result<String, IdentityError> {
    let mut value = vec![0_u8; bytes];
    fill(&mut value).map_err(IdentityError::Random)?;
    Ok(hex(&value))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn valid_installation_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// 安装身份加载、校验或首次生成失败。
///
/// 调用方只能将其投影为 Gateway 不可用，不能把路径、证书或密钥细节发送给设备。
#[derive(Debug, Error)]
pub(crate) enum IdentityError {
    #[error("device identity path is unsafe: {0}")]
    UnsafePath(PathBuf),
    #[error("device identity file permissions are unsafe: {0}")]
    UnsafePermissions(PathBuf),
    #[error("device identity metadata is invalid")]
    InvalidMetadata,
    #[error("device certificate is invalid")]
    InvalidCertificate,
    #[error("device certificate fingerprint does not match installation metadata")]
    FingerprintMismatch,
    #[error("device identity directory could not be created: {0}")]
    CreateDirectory(std::io::Error),
    #[error("device identity permissions could not be set: {0}")]
    Permissions(std::io::Error),
    #[error("device identity could not be read: {0}")]
    Read(std::io::Error),
    #[error("device identity could not be written: {0}")]
    Write(std::io::Error),
    #[error("device identity could not be published: {0}")]
    Publish(std::io::Error),
    #[error("device identity document is invalid: {0}")]
    Document(serde_json::Error),
    #[error("device certificate could not be generated: {0}")]
    Generate(rcgen::Error),
    #[error("secure random generation failed: {0}")]
    Random(getrandom::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_identity_is_private_stable_and_detects_tampering() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let first = InstallationIdentity::load_or_create(temporary.path()).expect("create");
        let second = InstallationIdentity::load_or_create(temporary.path()).expect("reload");
        assert_eq!(first.installation_id, second.installation_id);
        assert_eq!(
            first.certificate_fingerprint,
            second.certificate_fingerprint
        );
        let device_directory = temporary.path().join(DEVICE_DIRECTORY);
        for name in [INSTALLATION_FILE, CERTIFICATE_FILE, PRIVATE_KEY_FILE] {
            let mode = fs::metadata(device_directory.join(name))
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }

        fs::write(device_directory.join(CERTIFICATE_FILE), b"tampered").expect("tamper");
        assert!(matches!(
            InstallationIdentity::load_or_create(temporary.path()),
            Err(IdentityError::InvalidCertificate)
        ));
    }
}
