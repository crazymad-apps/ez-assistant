//! RFC 9382 配对、Ed25519 长期认证与 transcript 绑定辅助。

use assistant_protocol::{DeviceCapabilitiesSnapshot, OutputPreferenceSnapshot};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use getrandom::fill;
use hmac::{Hmac, Mac};
use pakery_core::crypto::CpaceGroup;
use pakery_crypto::{P256Group, Spake2P256};
use pakery_spake2::{PartyB, PartyBState, Spake2Output};
use rand_core::{OsRng, UnwrapErr};
use sha2::{Digest, Sha256, Sha512};
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

/// Host 侧尚未完成的单次 SPAKE2 握手状态；消费后不可重用。
pub(super) struct HostPakeState {
    state: PartyBState<Spake2P256>,
}

/// 配对码握手导出的临时密钥材料，用于确认双方和绑定长期设备公钥。
///
/// 该值只存在于当前配对连接，不能作为持久设备密钥保存。
pub(super) struct PairingKeys {
    output: Spake2Output,
}

impl HostPakeState {
    pub(super) fn start(
        pairing_code: &str,
        pairing_request_id: &str,
        installation_id: &str,
        associated_data: &[u8],
    ) -> Result<(Self, Vec<u8>), CryptoError> {
        if pairing_code.len() != 6 || !pairing_code.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(CryptoError::InvalidPairingCode);
        }
        let scalar = password_scalar(pairing_code, pairing_request_id, installation_id)?;
        let mut rng = UnwrapErr(OsRng);
        let (share, state) = PartyB::<Spake2P256>::start(
            &scalar,
            pairing_request_id.as_bytes(),
            installation_id.as_bytes(),
            associated_data,
            &mut rng,
        )
        .map_err(|_| CryptoError::PakeFailed)?;
        Ok((Self { state }, share))
    }

    pub(super) fn finish(self, device_share: &[u8]) -> Result<PairingKeys, CryptoError> {
        let output = self
            .state
            .finish(device_share)
            .map_err(|_| CryptoError::PakeFailed)?;
        Ok(PairingKeys { output })
    }

    #[cfg(test)]
    fn start_with_scalar(
        pairing_code: &str,
        pairing_request_id: &str,
        installation_id: &str,
        associated_data: &[u8],
        scalar_bytes: &[u8; 32],
    ) -> Result<(Self, Vec<u8>), CryptoError> {
        let password = password_scalar(pairing_code, pairing_request_id, installation_id)?;
        let mut wide = [0_u8; 64];
        wide[32..].copy_from_slice(scalar_bytes);
        let scalar =
            P256Group::scalar_from_wide_bytes(&wide).map_err(|_| CryptoError::PakeFailed)?;
        let (share, state) = PartyB::<Spake2P256>::start_with_scalar(
            &password,
            &scalar,
            pairing_request_id.as_bytes(),
            installation_id.as_bytes(),
            associated_data,
        )
        .map_err(|_| CryptoError::PakeFailed)?;
        Ok((Self { state }, share))
    }
}

impl PairingKeys {
    pub(super) fn host_confirmation(&self) -> &[u8] {
        &self.output.confirmation_mac
    }

    pub(super) fn verify_device_confirmation(&self, value: &[u8]) -> Result<(), CryptoError> {
        self.output
            .verify_peer_confirmation(value)
            .map_err(|_| CryptoError::ConfirmationFailed)
    }

    pub(super) fn binding_mac(&self, label: &[u8], transcript: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(self.output.session_key.as_bytes())
            .expect("HMAC accepts any key length");
        mac.update(label);
        mac.update(transcript);
        mac.finalize().into_bytes().to_vec()
    }

    pub(super) fn verify_binding_mac(
        &self,
        label: &[u8],
        transcript: &[u8],
        candidate: &[u8],
    ) -> Result<(), CryptoError> {
        let mut mac = HmacSha256::new_from_slice(self.output.session_key.as_bytes())
            .expect("HMAC accepts any key length");
        mac.update(label);
        mac.update(transcript);
        mac.verify_slice(candidate)
            .map_err(|_| CryptoError::BindingFailed)
    }
}

pub(super) fn pairing_associated_data(
    pairing_request_id: &str,
    installation_id: &str,
    certificate_fingerprint: &str,
    device_nonce: &str,
    host_nonce: &str,
    capabilities: DeviceCapabilitiesSnapshot,
) -> Vec<u8> {
    transcript(&[
        b"ez-assistant-pairing-v1",
        pairing_request_id.as_bytes(),
        installation_id.as_bytes(),
        certificate_fingerprint.as_bytes(),
        device_nonce.as_bytes(),
        host_nonce.as_bytes(),
        &capability_bytes(capabilities),
    ])
}

pub(super) fn pairing_bind_transcript(associated_data: &[u8], public_key: &[u8]) -> Vec<u8> {
    transcript(&[b"ez-assistant-bind-v1", associated_data, public_key])
}

pub(super) fn pairing_commit_transcript(bind_transcript: &[u8], device_id: &str) -> Vec<u8> {
    transcript(&[
        b"ez-assistant-commit-v1",
        bind_transcript,
        device_id.as_bytes(),
    ])
}

pub(super) fn auth_transcript(
    connection_id: &str,
    host_nonce: &str,
    device_id: &str,
    device_nonce: &str,
    capabilities: DeviceCapabilitiesSnapshot,
    preference: OutputPreferenceSnapshot,
) -> Vec<u8> {
    let preference = match preference {
        OutputPreferenceSnapshot::Text => b"text" as &[u8],
        OutputPreferenceSnapshot::Audio => b"audio",
        OutputPreferenceSnapshot::TextAndAudio => b"text_and_audio",
    };
    transcript(&[
        b"ez-assistant-auth-v1",
        &1_u16.to_be_bytes(),
        &0_u16.to_be_bytes(),
        connection_id.as_bytes(),
        host_nonce.as_bytes(),
        device_id.as_bytes(),
        device_nonce.as_bytes(),
        &capability_bytes(capabilities),
        preference,
    ])
}

pub(super) fn verify_ed25519(
    public_key: &[u8],
    transcript: &[u8],
    signature: &[u8],
) -> Result<(), CryptoError> {
    let public_key: [u8; 32] = public_key
        .try_into()
        .map_err(|_| CryptoError::InvalidPublicKey)?;
    let signature = Signature::from_slice(signature).map_err(|_| CryptoError::InvalidSignature)?;
    VerifyingKey::from_bytes(&public_key)
        .map_err(|_| CryptoError::InvalidPublicKey)?
        .verify_strict(transcript, &signature)
        .map_err(|_| CryptoError::InvalidSignature)
}

pub(super) fn decode_base64(value: &str, expected_length: usize) -> Result<Vec<u8>, CryptoError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| CryptoError::InvalidEncoding)?;
    if decoded.len() != expected_length {
        return Err(CryptoError::InvalidEncoding);
    }
    Ok(decoded)
}

pub(super) fn encode_base64(value: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(value)
}

pub(super) fn random_token(bytes: usize) -> Result<String, CryptoError> {
    let mut value = vec![0_u8; bytes];
    fill(&mut value).map_err(CryptoError::Random)?;
    Ok(encode_base64(&value))
}

pub(super) fn random_stream_id() -> Result<u32, CryptoError> {
    loop {
        let mut value = [0_u8; 4];
        fill(&mut value).map_err(CryptoError::Random)?;
        let value = u32::from_be_bytes(value);
        if value != 0 {
            return Ok(value);
        }
    }
}

fn password_scalar(
    pairing_code: &str,
    pairing_request_id: &str,
    installation_id: &str,
) -> Result<<P256Group as CpaceGroup>::Scalar, CryptoError> {
    let input = transcript(&[
        b"ez-assistant-spake2-password-v1",
        pairing_code.as_bytes(),
        pairing_request_id.as_bytes(),
        installation_id.as_bytes(),
    ]);
    let wide = Sha512::digest(input);
    P256Group::scalar_from_wide_bytes(&wide).map_err(|_| CryptoError::PakeFailed)
}

fn transcript(parts: &[&[u8]]) -> Vec<u8> {
    let mut output = Vec::new();
    for part in parts {
        output.extend_from_slice(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        output.extend_from_slice(part);
    }
    output
}

fn capability_bytes(capabilities: DeviceCapabilitiesSnapshot) -> [u8; 7] {
    [
        u8::from(capabilities.input_text),
        u8::from(capabilities.input_pcm16_16k_mono),
        u8::from(capabilities.output_text),
        u8::from(capabilities.output_pcm16_16k_mono),
        u8::from(capabilities.playback_cancel),
        u8::from(capabilities.display_status),
        u8::from(capabilities.display_transcript),
    ]
}

/// 配对、长期签名与 transcript 绑定校验的内部错误。
#[derive(Debug, Error)]
pub(super) enum CryptoError {
    #[error("pairing code must contain exactly six digits")]
    InvalidPairingCode,
    #[error("SPAKE2 exchange failed")]
    PakeFailed,
    #[error("SPAKE2 confirmation failed")]
    ConfirmationFailed,
    #[error("pairing binding failed")]
    BindingFailed,
    #[error("public key is invalid")]
    InvalidPublicKey,
    #[error("signature is invalid")]
    InvalidSignature,
    #[error("wire cryptographic value is invalid")]
    InvalidEncoding,
    #[error("secure random generation failed: {0}")]
    Random(getrandom::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn transcript_signatures_are_bound_and_strict() {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let transcript = auth_transcript(
            "connection",
            "host",
            "device",
            "device-nonce",
            DeviceCapabilitiesSnapshot {
                input_text: true,
                output_text: true,
                ..DeviceCapabilitiesSnapshot::default()
            },
            OutputPreferenceSnapshot::Text,
        );
        let signature = signing_key.sign(&transcript);
        verify_ed25519(
            signing_key.verifying_key().as_bytes(),
            &transcript,
            &signature.to_bytes(),
        )
        .expect("verify");
        assert!(
            verify_ed25519(
                signing_key.verifying_key().as_bytes(),
                b"different",
                &signature.to_bytes(),
            )
            .is_err()
        );
    }

    #[test]
    fn shared_node_rust_spake2_and_authentication_fixture_matches() {
        #[derive(serde::Deserialize)]
        struct Fixture {
            pairing: PairingFixture,
            authentication: AuthenticationFixture,
        }
        #[derive(serde::Deserialize)]
        struct PairingFixture {
            pairing_code: String,
            pairing_request_id: String,
            installation_id: String,
            certificate_fingerprint: String,
            device_nonce: String,
            host_nonce: String,
            capabilities: DeviceCapabilitiesSnapshot,
            host_scalar_hex: String,
            associated_data: String,
            device_share: String,
            host_share: String,
            device_confirmation: String,
            host_confirmation: String,
            session_key: String,
        }
        #[derive(serde::Deserialize)]
        struct AuthenticationFixture {
            connection_id: String,
            host_nonce: String,
            device_id: String,
            device_nonce: String,
            output_preference: OutputPreferenceSnapshot,
            transcript: String,
            public_key: String,
            signature: String,
        }

        let fixture: Fixture = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/resources/device-protocol-v1/fixtures/crypto-v1.json"
        )))
        .expect("fixture");
        let pairing = fixture.pairing;
        let associated_data = pairing_associated_data(
            &pairing.pairing_request_id,
            &pairing.installation_id,
            &pairing.certificate_fingerprint,
            &pairing.device_nonce,
            &pairing.host_nonce,
            pairing.capabilities,
        );
        assert_eq!(encode_base64(&associated_data), pairing.associated_data);
        let device_share = decode_base64(&pairing.device_share, 65).expect("device share");
        let scalar = hex_32(&pairing.host_scalar_hex);
        let (state, host_share) = HostPakeState::start_with_scalar(
            &pairing.pairing_code,
            &pairing.pairing_request_id,
            &pairing.installation_id,
            &associated_data,
            &scalar,
        )
        .expect("start");
        let keys = state.finish(&device_share).expect("finish");
        assert_eq!(encode_base64(&host_share), pairing.host_share);
        assert_eq!(
            encode_base64(keys.host_confirmation()),
            pairing.host_confirmation
        );
        assert_eq!(
            encode_base64(keys.output.session_key.as_bytes()),
            pairing.session_key
        );
        keys.verify_device_confirmation(
            &decode_base64(&pairing.device_confirmation, 32).expect("device confirmation"),
        )
        .expect("device confirmation");

        let authentication = fixture.authentication;
        let transcript = auth_transcript(
            &authentication.connection_id,
            &authentication.host_nonce,
            &authentication.device_id,
            &authentication.device_nonce,
            pairing.capabilities,
            authentication.output_preference,
        );
        assert_eq!(encode_base64(&transcript), authentication.transcript);
        verify_ed25519(
            &decode_base64(&authentication.public_key, 32).expect("public key"),
            &transcript,
            &decode_base64(&authentication.signature, 64).expect("signature"),
        )
        .expect("authentication signature");
    }

    fn hex_32(value: &str) -> [u8; 32] {
        assert_eq!(value.len(), 64);
        let mut output = [0_u8; 32];
        for (index, byte) in output.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).expect("hex");
        }
        output
    }
}
