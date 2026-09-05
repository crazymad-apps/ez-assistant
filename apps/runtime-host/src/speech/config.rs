//! 从 Runtime Home 的同一份安全配置文档编译 Host 私有语音能力。

use std::{path::PathBuf, sync::Arc, time::Duration};

use assistant_protocol::{DeviceSpeechServicesSnapshot, SpeechServiceStatusSnapshot};
use assistant_runtime::{ConfigSourceLoad, RuntimeConfigSource};
use serde::Deserialize;

use super::provider::{
    AsrProvider, DashScopeSpeechRecognizer, DashScopeSpeechSynthesizer, TtsProvider,
};

const DEFAULT_DASHSCOPE_ENDPOINT: &str = "https://dashscope.aliyuncs.com";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 120_000;

/// 从用户配置成功编译出的 Host 私有语音依赖。
///
/// ASR/TTS 分别编译并独立降级；凭据只保留在 Provider 内，不进入公共快照。
pub(super) struct CompiledSpeechConfig {
    pub(super) asr: Option<Arc<dyn AsrProvider>>,
    pub(super) tts: Option<Arc<dyn TtsProvider>>,
    pub(super) status: DeviceSpeechServicesSnapshot,
    pub(super) debug_audio_directory: Option<PathBuf>,
    pub(super) asr_timeout: Duration,
    pub(super) tts_timeout: Duration,
}

impl CompiledSpeechConfig {
    pub(super) fn unavailable() -> Self {
        Self {
            asr: None,
            tts: None,
            status: DeviceSpeechServicesSnapshot::default(),
            debug_audio_directory: None,
            asr_timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
            tts_timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
        }
    }
}

/// 顶层配置文档中与本模块相关的可选片段。
#[derive(Deserialize)]
struct SpeechDocument {
    speech: Option<RawSpeechConfig>,
}

/// `[speech]` 的严格反序列化形式。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSpeechConfig {
    // 先隔离原始表，单能力字段缺失/类型错误不能令另一能力一起反序列化失败。
    asr: Option<toml::Value>,
    tts: Option<toml::Value>,
    debug_audio_directory: Option<PathBuf>,
}

/// `[speech.asr]` 的首版 Provider 配置。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAsrConfig {
    provider: String,
    model: String,
    credential: String,
    #[serde(default = "default_endpoint")]
    endpoint: String,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

/// `[speech.tts]` 的首版 Provider 配置。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTtsConfig {
    provider: String,
    model: String,
    voice: String,
    credential: String,
    #[serde(default = "default_endpoint")]
    endpoint: String,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

pub(super) async fn load(source: &dyn RuntimeConfigSource) -> CompiledSpeechConfig {
    let ConfigSourceLoad::Document(document) = source.load().await else {
        return CompiledSpeechConfig::unavailable();
    };
    let Ok(document) = toml::from_str::<SpeechDocument>(document.contents()) else {
        return CompiledSpeechConfig::unavailable();
    };
    let Some(speech) = document.speech else {
        return CompiledSpeechConfig::unavailable();
    };

    let raw_asr = speech
        .asr
        .and_then(|raw| raw.try_into::<RawAsrConfig>().ok());
    let raw_tts = speech
        .tts
        .and_then(|raw| raw.try_into::<RawTtsConfig>().ok());
    let asr_timeout = Duration::from_millis(
        raw_asr
            .as_ref()
            .map_or(DEFAULT_TIMEOUT_MS, |raw| raw.timeout_ms),
    );
    let tts_timeout = Duration::from_millis(
        raw_tts
            .as_ref()
            .map_or(DEFAULT_TIMEOUT_MS, |raw| raw.timeout_ms),
    );
    let asr = raw_asr.and_then(compile_asr);
    let tts = raw_tts.and_then(compile_tts);
    CompiledSpeechConfig {
        status: DeviceSpeechServicesSnapshot {
            asr: if asr.is_some() {
                SpeechServiceStatusSnapshot::Ready
            } else {
                SpeechServiceStatusSnapshot::Unavailable
            },
            tts: if tts.is_some() {
                SpeechServiceStatusSnapshot::Ready
            } else {
                SpeechServiceStatusSnapshot::Unavailable
            },
        },
        asr,
        tts,
        debug_audio_directory: debug_audio_directory(speech.debug_audio_directory),
        asr_timeout,
        tts_timeout,
    }
}

fn compile_asr(raw: RawAsrConfig) -> Option<Arc<dyn AsrProvider>> {
    if raw.provider != "dashscope"
        || !valid_text(&raw.model)
        || !valid_text(&raw.credential)
        || !valid_timeout(raw.timeout_ms)
    {
        return None;
    }
    DashScopeSpeechRecognizer::new(
        raw.endpoint,
        raw.model,
        raw.credential,
        Duration::from_millis(raw.timeout_ms),
    )
    .ok()
    .map(|provider| Arc::new(provider) as Arc<dyn AsrProvider>)
}

fn compile_tts(raw: RawTtsConfig) -> Option<Arc<dyn TtsProvider>> {
    if raw.provider != "dashscope"
        || !valid_text(&raw.model)
        || !valid_text(&raw.voice)
        || !valid_text(&raw.credential)
        || !valid_timeout(raw.timeout_ms)
    {
        return None;
    }
    DashScopeSpeechSynthesizer::new(
        raw.endpoint,
        raw.model,
        raw.voice,
        raw.credential,
        Duration::from_millis(raw.timeout_ms),
    )
    .ok()
    .map(|provider| Arc::new(provider) as Arc<dyn TtsProvider>)
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.trim() == value
}

fn valid_timeout(value: u64) -> bool {
    value > 0 && value <= MAX_TIMEOUT_MS
}

#[cfg(test)]
fn valid_endpoint(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            || (url.scheme() == "http"
                && url
                    .host_str()
                    .is_some_and(|host| host == "127.0.0.1" || host == "localhost"))
    })
}

fn debug_audio_directory(value: Option<PathBuf>) -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        value.filter(|path| path.is_absolute())
    } else {
        None
    }
}

fn default_endpoint() -> String {
    DEFAULT_DASHSCOPE_ENDPOINT.to_owned()
}

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

#[cfg(test)]
mod tests {
    use super::*;
    use assistant_runtime::{
        ConfigDocument, ConfigSourceFuture, ConfigSourceLoad, RuntimeConfigSource,
    };

    struct StaticSource(&'static str);

    impl RuntimeConfigSource for StaticSource {
        fn load(&self) -> ConfigSourceFuture<'_> {
            Box::pin(std::future::ready(ConfigSourceLoad::Document(
                ConfigDocument::new(self.0.to_owned(), "test".to_owned()),
            )))
        }
    }

    #[test]
    fn endpoint_only_allows_https_or_loopback_http() {
        assert!(valid_endpoint("https://dashscope.aliyuncs.com"));
        assert!(valid_endpoint("http://127.0.0.1:1234"));
        assert!(!valid_endpoint("http://example.com"));
    }

    #[test]
    fn debug_audio_capture_requires_an_absolute_path_and_is_disabled_in_release() {
        assert_eq!(debug_audio_directory(Some(PathBuf::from("audio"))), None);

        let absolute = std::env::temp_dir().join("ez-assistant-debug-audio");
        if cfg!(debug_assertions) {
            assert_eq!(
                debug_audio_directory(Some(absolute.clone())),
                Some(absolute)
            );
        } else {
            assert_eq!(debug_audio_directory(Some(absolute)), None);
        }
    }

    #[tokio::test]
    async fn valid_asr_and_tts_compile_as_parallel_capabilities() {
        let source = StaticSource(
            r#"
[speech.asr]
provider = "dashscope"
model = "qwen-audio-3.0-asr-flash"
credential = "secret"
timeout_ms = 30000

[speech.tts]
provider = "dashscope"
model = "qwen-audio-3.0-tts-flash"
voice = "longanhuan_v3.6"
credential = "secret"
timeout_ms = 30000
"#,
        );
        let compiled = load(&source).await;
        assert!(compiled.asr.is_some());
        assert!(compiled.tts.is_some());
        assert_eq!(compiled.status.asr, SpeechServiceStatusSnapshot::Ready);
        assert_eq!(compiled.status.tts, SpeechServiceStatusSnapshot::Ready);
    }

    #[tokio::test]
    async fn invalid_or_missing_speech_config_degrades_only_speech() {
        let invalid = load(&StaticSource(
            r#"
[speech.asr]
provider = "unknown"
model = "model"
credential = "secret"
timeout_ms = 30000
"#,
        ))
        .await;
        assert!(invalid.asr.is_none());
        assert_eq!(invalid.status, DeviceSpeechServicesSnapshot::default());
        let missing = load(&StaticSource("schema_version = 1")).await;
        assert_eq!(missing.status, DeviceSpeechServicesSnapshot::default());
    }

    #[tokio::test]
    async fn malformed_provider_table_does_not_disable_the_other_capability() {
        for invalid in [
            "provider = 42",
            "provider = 'dashscope'",
            "unknown = true",
            "timeout_ms = 'bad'",
        ] {
            let asr_valid = format!(
                "[speech.asr]\nprovider = 'dashscope'\nmodel = 'asr'\ncredential = 'fixture'\n[speech.tts]\n{invalid}\n"
            );
            let tts_valid = format!(
                "[speech.tts]\nprovider = 'dashscope'\nmodel = 'tts'\nvoice = 'voice'\ncredential = 'fixture'\n[speech.asr]\n{invalid}\n"
            );
            struct OwnedSource(String);
            impl RuntimeConfigSource for OwnedSource {
                fn load(&self) -> ConfigSourceFuture<'_> {
                    Box::pin(std::future::ready(ConfigSourceLoad::Document(
                        ConfigDocument::new(self.0.clone(), "test".to_owned()),
                    )))
                }
            }
            let compiled = load(&OwnedSource(asr_valid)).await;
            assert_eq!(compiled.status.asr, SpeechServiceStatusSnapshot::Ready);
            assert_eq!(
                compiled.status.tts,
                SpeechServiceStatusSnapshot::Unavailable
            );
            let compiled = load(&OwnedSource(tts_valid)).await;
            assert_eq!(
                compiled.status.asr,
                SpeechServiceStatusSnapshot::Unavailable
            );
            assert_eq!(compiled.status.tts, SpeechServiceStatusSnapshot::Ready);
        }
    }
}
