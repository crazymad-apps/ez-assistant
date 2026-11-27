//! Host 私有语音 Provider、配置编译与受控请求生命周期。

mod config;
mod provider;
mod service;

pub(crate) use provider::wav_bytes_to_pcm16_16k_mono;
pub(crate) use service::{SpeechService, SpeechServiceError, SpeechServiceHandle, SpeechSlots};
