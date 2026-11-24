//! 从 Runtime Home 的同一份安全配置文档编译、读取与 CAS 更新 Host 私有语音能力。

use std::{path::PathBuf, sync::Arc, time::Duration};

use assistant_protocol::{
    AsrConfigurationSnapshot, DeviceSpeechServicesSnapshot, RuntimeErrorCode, RuntimeErrorInfo,
    SpeechConfigurationSnapshot, SpeechProviderKind, SpeechServiceStatusSnapshot,
    TtsConfigurationSnapshot, UpdateAsrConfigurationInput, UpdateSpeechConfigurationRequest,
    UpdateTtsConfigurationInput,
};
use assistant_runtime::{ConfigSourceLoad, ConfigSourceReplace, RuntimeConfigSource};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use toml_edit::{DocumentMut, Item, Table};

use super::provider::{
    AsrProvider, DashScopeSpeechRecognizer, DashScopeSpeechSynthesizer, MiniMaxSpeechSynthesizer,
    OpenAiSpeechRecognizer, OpenAiSpeechSynthesizer, TtsProvider,
};

const DEFAULT_DASHSCOPE_ENDPOINT: &str = "https://dashscope.aliyuncs.com";
const DEFAULT_SILICONFLOW_ENDPOINT: &str = "https://api.siliconflow.cn/v1";
const DEFAULT_ZHIPU_ENDPOINT: &str = "https://open.bigmodel.cn/api/paas/v4";
const DEFAULT_MINIMAX_ENDPOINT: &str = "https://api.minimaxi.com/v1/t2a_v2";
const DEFAULT_OPENAI_ENDPOINT: &str = "https://api.openai.com/v1";
const DEFAULT_GROQ_ENDPOINT: &str = "https://api.groq.com/openai/v1";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 120_000;
const MAX_TEXT_BYTES: usize = 512;

/// 读取或更新 `config.toml` 中语音配置段时的稳定错误分类。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum SpeechConfigError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("语音配置在编辑期间已被其他操作修改，请刷新后重试。")]
    Conflict,
    #[error("语音配置文件当前不可用。")]
    Unavailable,
}

impl SpeechConfigError {
    pub(crate) fn to_protocol_info(&self) -> RuntimeErrorInfo {
        match self {
            Self::Invalid(message) => {
                RuntimeErrorInfo::new(RuntimeErrorCode::InvalidRequest, *message)
            }
            Self::Conflict => {
                RuntimeErrorInfo::new(RuntimeErrorCode::ConfigurationConflict, self.to_string())
            }
            Self::Unavailable => {
                RuntimeErrorInfo::new(RuntimeErrorCode::ConfigurationUnavailable, self.to_string())
            }
        }
    }
}

pub(super) struct SpeechConfigUpdateOutcome {
    pub(super) compiled: CompiledSpeechConfig,
    pub(super) configuration: SpeechConfigurationSnapshot,
}

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

/// `[speech.asr]` 的 Provider 配置。
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawAsrConfig {
    provider: String,
    model: String,
    credential: String,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

/// `[speech.tts]` 的 Provider 配置。
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawTtsConfig {
    provider: String,
    model: String,
    voice: String,
    credential: String,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

pub(super) async fn load(source: &dyn RuntimeConfigSource) -> CompiledSpeechConfig {
    let ConfigSourceLoad::Document(document) = source.load().await else {
        return CompiledSpeechConfig::unavailable();
    };
    compile_document_contents(document.contents())
}

fn compile_document_contents(contents: &str) -> CompiledSpeechConfig {
    let Ok(document) = toml::from_str::<SpeechDocument>(contents) else {
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

pub(super) async fn read_configuration(
    source: &dyn RuntimeConfigSource,
    status: DeviceSpeechServicesSnapshot,
) -> Result<SpeechConfigurationSnapshot, SpeechConfigError> {
    match source.load().await {
        ConfigSourceLoad::Missing => Ok(project_configuration(None, None, None, status)),
        ConfigSourceLoad::Document(doc) => {
            let revision = doc.revision().to_owned();
            let parsed = toml::from_str::<SpeechDocument>(doc.contents()).ok();
            let speech = parsed.and_then(|d| d.speech);
            let raw_asr = speech
                .as_ref()
                .and_then(|s| s.asr.clone())
                .and_then(|v| v.try_into::<RawAsrConfig>().ok());
            let raw_tts = speech
                .as_ref()
                .and_then(|s| s.tts.clone())
                .and_then(|v| v.try_into::<RawTtsConfig>().ok());
            Ok(project_configuration(
                Some(revision),
                raw_asr.as_ref(),
                raw_tts.as_ref(),
                status,
            ))
        }
        ConfigSourceLoad::Unavailable(_) => Err(SpeechConfigError::Unavailable),
    }
}

pub(super) async fn update_configuration(
    source: &dyn RuntimeConfigSource,
    request: UpdateSpeechConfigurationRequest,
) -> Result<SpeechConfigUpdateOutcome, SpeechConfigError> {
    let (mut document, current_revision) = match source.load().await {
        ConfigSourceLoad::Missing => (
            "schema_version = 1\n"
                .parse::<DocumentMut>()
                .expect("static TOML"),
            None,
        ),
        ConfigSourceLoad::Document(doc) => (
            doc.contents()
                .parse::<DocumentMut>()
                .map_err(|_| SpeechConfigError::Invalid("现有配置文件不是合法 TOML。"))?,
            Some(doc.revision().to_owned()),
        ),
        ConfigSourceLoad::Unavailable(_) => return Err(SpeechConfigError::Unavailable),
    };

    if request.expected_revision != current_revision {
        return Err(SpeechConfigError::Conflict);
    }

    let existing_parsed = toml::from_str::<SpeechDocument>(&document.to_string()).ok();
    let existing_speech = existing_parsed.and_then(|d| d.speech);
    let existing_asr = existing_speech
        .as_ref()
        .and_then(|s| s.asr.clone())
        .and_then(|v| v.try_into::<RawAsrConfig>().ok());
    let existing_tts = existing_speech
        .as_ref()
        .and_then(|s| s.tts.clone())
        .and_then(|v| v.try_into::<RawTtsConfig>().ok());

    let next_asr = build_next_asr(&request.asr, existing_asr.as_ref())?;
    let reuse_asr_secret = request
        .asr
        .credential
        .as_ref()
        .map(|s| s.expose().trim())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| next_asr.as_ref().map(|a| a.credential.clone()))
        .or_else(|| {
            (!request.asr.clear_credential)
                .then(|| existing_asr.as_ref().map(|a| a.credential.clone()))
                .flatten()
        });
    let next_tts = build_next_tts(
        &request.tts,
        existing_tts.as_ref(),
        reuse_asr_secret.as_deref(),
    )?;

    if next_asr.is_none() && next_tts.is_none() {
        if let Some(speech_table) = document.get_mut("speech").and_then(Item::as_table_mut) {
            speech_table.remove("asr");
            speech_table.remove("tts");
            if speech_table.is_empty() {
                document.remove("speech");
            }
        }
    } else {
        if !document.contains_key("speech") {
            let mut tbl = Table::new();
            tbl.set_implicit(true);
            document["speech"] = Item::Table(tbl);
        }
        let speech_table = document
            .get_mut("speech")
            .and_then(Item::as_table_mut)
            .ok_or(SpeechConfigError::Invalid(
                "[speech] 配置节点必须是 TOML 表。",
            ))?;

        match &next_asr {
            Some(asr) => {
                let section = toml::to_string(asr)
                    .map_err(|_| SpeechConfigError::Unavailable)?
                    .parse::<DocumentMut>()
                    .map_err(|_| SpeechConfigError::Unavailable)?;
                speech_table["asr"] = Item::Table(section.as_table().clone());
            }
            None => {
                speech_table.remove("asr");
            }
        }
        match &next_tts {
            Some(tts) => {
                let section = toml::to_string(tts)
                    .map_err(|_| SpeechConfigError::Unavailable)?
                    .parse::<DocumentMut>()
                    .map_err(|_| SpeechConfigError::Unavailable)?;
                speech_table["tts"] = Item::Table(section.as_table().clone());
            }
            None => {
                speech_table.remove("tts");
            }
        }
    }

    let next_text = document.to_string();
    match source.replace(current_revision, next_text).await {
        ConfigSourceReplace::Applied(saved_doc) => {
            let compiled = compile_document_contents(saved_doc.contents());
            let configuration = project_configuration(
                Some(saved_doc.revision().to_owned()),
                next_asr.as_ref(),
                next_tts.as_ref(),
                compiled.status,
            );
            Ok(SpeechConfigUpdateOutcome {
                compiled,
                configuration,
            })
        }
        ConfigSourceReplace::Conflict(_) => Err(SpeechConfigError::Conflict),
        ConfigSourceReplace::Unavailable(_) => Err(SpeechConfigError::Unavailable),
    }
}

fn build_next_asr(
    input: &UpdateAsrConfigurationInput,
    existing: Option<&RawAsrConfig>,
) -> Result<Option<RawAsrConfig>, SpeechConfigError> {
    if !input.enabled {
        return Ok(None);
    }
    Ok(Some(validate_and_build_asr(input, existing)?))
}

fn validate_and_build_asr(
    input: &UpdateAsrConfigurationInput,
    existing: Option<&RawAsrConfig>,
) -> Result<RawAsrConfig, SpeechConfigError> {
    if input.provider == SpeechProviderKind::Minimax {
        return Err(SpeechConfigError::Invalid(
            "MiniMax 暂仅支持语音播放（TTS），语音识别请选择阿里云百炼、硅基流动、智谱、OpenAI 或 Groq。",
        ));
    }
    let endpoint = normalize_endpoint(&input.endpoint, input.provider)?;
    let model = normalize_required_text(&input.model, "请填写语音识别（ASR）模型名称。")?;
    if !valid_timeout(input.timeout_ms) {
        return Err(SpeechConfigError::Invalid(
            "ASR 超时时间必须在 1 到 120000 毫秒之间。",
        ));
    }
    let credential = resolve_next_credential(
        input.credential.as_ref().map(|s| s.expose()),
        input.clear_credential,
        existing.map(|c| c.credential.as_str()),
        "请填写或保留语音识别（ASR）API Key 凭据。",
    )?;
    let raw = RawAsrConfig {
        provider: provider_str(input.provider).to_owned(),
        model,
        credential,
        endpoint: Some(endpoint),
        timeout_ms: input.timeout_ms,
    };
    if compile_asr_ref(&raw).is_none() {
        return Err(SpeechConfigError::Invalid(
            "语音识别（ASR）配置无法通过 Provider 校验。",
        ));
    }
    Ok(raw)
}

fn build_next_tts(
    input: &UpdateTtsConfigurationInput,
    existing: Option<&RawTtsConfig>,
    reuse_asr_credential: Option<&str>,
) -> Result<Option<RawTtsConfig>, SpeechConfigError> {
    if !input.enabled {
        return Ok(None);
    }
    Ok(Some(validate_and_build_tts(
        input,
        existing,
        reuse_asr_credential,
    )?))
}

fn validate_and_build_tts(
    input: &UpdateTtsConfigurationInput,
    existing: Option<&RawTtsConfig>,
    reuse_asr_credential: Option<&str>,
) -> Result<RawTtsConfig, SpeechConfigError> {
    if input.provider == SpeechProviderKind::Groq {
        return Err(SpeechConfigError::Invalid(
            "Groq 暂仅支持语音识别（ASR），语音播放请选择阿里云百炼、硅基流动、智谱、MiniMax 或 OpenAI。",
        ));
    }
    let endpoint = normalize_endpoint(&input.endpoint, input.provider)?;
    let model = normalize_required_text(&input.model, "请填写语音播放（TTS）模型名称。")?;
    let voice = normalize_required_text(&input.voice, "请填写或选择语音播放（TTS）音色。")?;
    if !valid_timeout(input.timeout_ms) {
        return Err(SpeechConfigError::Invalid(
            "TTS 超时时间必须在 1 到 120000 毫秒之间。",
        ));
    }
    let credential = resolve_next_tts_credential(
        input.credential.as_ref().map(|s| s.expose()),
        input.clear_credential,
        input.reuse_asr_credential,
        existing.map(|c| c.credential.as_str()),
        reuse_asr_credential,
    )?;
    let raw = RawTtsConfig {
        provider: provider_str(input.provider).to_owned(),
        model,
        voice,
        credential,
        endpoint: Some(endpoint),
        timeout_ms: input.timeout_ms,
    };
    if compile_tts_ref(&raw).is_none() {
        return Err(SpeechConfigError::Invalid(
            "语音播放（TTS）配置无法通过 Provider 校验。",
        ));
    }
    Ok(raw)
}

pub(super) async fn compile_asr_draft(
    source: &dyn RuntimeConfigSource,
    input: &UpdateAsrConfigurationInput,
) -> Result<(Arc<dyn AsrProvider>, Duration), SpeechConfigError> {
    let existing_asr = load_existing_sections(source).await.0;
    let raw = validate_and_build_asr(input, existing_asr.as_ref())?;
    let timeout = Duration::from_millis(raw.timeout_ms);
    let provider =
        compile_asr_ref(&raw).ok_or(SpeechConfigError::Invalid("语音识别（ASR）配置无效。"))?;
    Ok((provider, timeout))
}

pub(super) async fn compile_tts_draft(
    source: &dyn RuntimeConfigSource,
    input: &UpdateTtsConfigurationInput,
    fallback_asr_credential: Option<&str>,
) -> Result<(Arc<dyn TtsProvider>, Duration), SpeechConfigError> {
    let (existing_asr, existing_tts) = load_existing_sections(source).await;
    let resolved_asr = fallback_asr_credential
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| existing_asr.map(|a| a.credential));
    let raw = validate_and_build_tts(input, existing_tts.as_ref(), resolved_asr.as_deref())?;
    let timeout = Duration::from_millis(raw.timeout_ms);
    let provider =
        compile_tts_ref(&raw).ok_or(SpeechConfigError::Invalid("语音播放（TTS）配置无效。"))?;
    Ok((provider, timeout))
}

async fn load_existing_sections(
    source: &dyn RuntimeConfigSource,
) -> (Option<RawAsrConfig>, Option<RawTtsConfig>) {
    let ConfigSourceLoad::Document(doc) = source.load().await else {
        return (None, None);
    };
    let parsed = toml::from_str::<SpeechDocument>(doc.contents()).ok();
    let speech = parsed.and_then(|d| d.speech);
    let existing_asr = speech
        .as_ref()
        .and_then(|s| s.asr.clone())
        .and_then(|v| v.try_into::<RawAsrConfig>().ok());
    let existing_tts = speech
        .and_then(|s| s.tts)
        .and_then(|v| v.try_into::<RawTtsConfig>().ok());
    (existing_asr, existing_tts)
}

fn project_configuration(
    revision: Option<String>,
    asr: Option<&RawAsrConfig>,
    tts: Option<&RawTtsConfig>,
    status: DeviceSpeechServicesSnapshot,
) -> SpeechConfigurationSnapshot {
    let asr_snapshot = match asr {
        Some(cfg) => {
            let provider =
                parse_provider_kind(&cfg.provider).unwrap_or(SpeechProviderKind::Dashscope);
            let default_ep = default_endpoint_for(provider);
            AsrConfigurationSnapshot {
                enabled: true,
                provider,
                endpoint: cfg
                    .endpoint
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(default_ep)
                    .to_owned(),
                model: cfg.model.clone(),
                timeout_ms: cfg.timeout_ms,
                credential_configured: !cfg.credential.trim().is_empty(),
                credential_masked: mask_secret(&cfg.credential),
            }
        }
        None => AsrConfigurationSnapshot {
            enabled: false,
            provider: SpeechProviderKind::Dashscope,
            endpoint: DEFAULT_DASHSCOPE_ENDPOINT.to_owned(),
            model: String::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            credential_configured: false,
            credential_masked: None,
        },
    };
    let tts_snapshot = match tts {
        Some(cfg) => {
            let provider =
                parse_provider_kind(&cfg.provider).unwrap_or(SpeechProviderKind::Dashscope);
            let default_ep = default_endpoint_for(provider);
            TtsConfigurationSnapshot {
                enabled: true,
                provider,
                endpoint: cfg
                    .endpoint
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(default_ep)
                    .to_owned(),
                model: cfg.model.clone(),
                voice: cfg.voice.clone(),
                timeout_ms: cfg.timeout_ms,
                credential_configured: !cfg.credential.trim().is_empty(),
                credential_masked: mask_secret(&cfg.credential),
            }
        }
        None => TtsConfigurationSnapshot {
            enabled: false,
            provider: SpeechProviderKind::Dashscope,
            endpoint: DEFAULT_DASHSCOPE_ENDPOINT.to_owned(),
            model: String::new(),
            voice: String::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            credential_configured: false,
            credential_masked: None,
        },
    };
    SpeechConfigurationSnapshot {
        revision,
        asr: asr_snapshot,
        tts: tts_snapshot,
        status,
    }
}

fn compile_asr(raw: RawAsrConfig) -> Option<Arc<dyn AsrProvider>> {
    compile_asr_ref(&raw)
}

fn compile_asr_ref(raw: &RawAsrConfig) -> Option<Arc<dyn AsrProvider>> {
    let provider = parse_provider_kind(&raw.provider)?;
    if !valid_text(&raw.model) || !valid_text(&raw.credential) || !valid_timeout(raw.timeout_ms) {
        return None;
    }
    let endpoint = raw
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_endpoint_for(provider))
        .to_owned();
    let timeout = Duration::from_millis(raw.timeout_ms);
    match provider {
        SpeechProviderKind::Dashscope => DashScopeSpeechRecognizer::new(
            endpoint,
            raw.model.clone(),
            raw.credential.clone(),
            timeout,
        )
        .ok()
        .map(|p| Arc::new(p) as Arc<dyn AsrProvider>),
        SpeechProviderKind::Siliconflow
        | SpeechProviderKind::Zhipu
        | SpeechProviderKind::Openai
        | SpeechProviderKind::Groq => OpenAiSpeechRecognizer::new(
            endpoint,
            raw.model.clone(),
            raw.credential.clone(),
            timeout,
        )
        .ok()
        .map(|p| Arc::new(p) as Arc<dyn AsrProvider>),
        SpeechProviderKind::Minimax => None,
    }
}

fn compile_tts(raw: RawTtsConfig) -> Option<Arc<dyn TtsProvider>> {
    compile_tts_ref(&raw)
}

fn compile_tts_ref(raw: &RawTtsConfig) -> Option<Arc<dyn TtsProvider>> {
    let provider = parse_provider_kind(&raw.provider)?;
    if !valid_text(&raw.model)
        || !valid_text(&raw.voice)
        || !valid_text(&raw.credential)
        || !valid_timeout(raw.timeout_ms)
    {
        return None;
    }
    let endpoint = raw
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_endpoint_for(provider))
        .to_owned();
    let timeout = Duration::from_millis(raw.timeout_ms);
    let voice = if provider == SpeechProviderKind::Siliconflow && !raw.voice.contains(':') {
        format!("{}:{}", raw.model, raw.voice)
    } else {
        raw.voice.clone()
    };
    match provider {
        SpeechProviderKind::Dashscope => DashScopeSpeechSynthesizer::new(
            endpoint,
            raw.model.clone(),
            voice,
            raw.credential.clone(),
            timeout,
        )
        .ok()
        .map(|p| Arc::new(p) as Arc<dyn TtsProvider>),
        SpeechProviderKind::Siliconflow
        | SpeechProviderKind::Zhipu
        | SpeechProviderKind::Openai => OpenAiSpeechSynthesizer::new(
            endpoint,
            raw.model.clone(),
            voice,
            raw.credential.clone(),
            timeout,
        )
        .ok()
        .map(|p| Arc::new(p) as Arc<dyn TtsProvider>),
        SpeechProviderKind::Minimax => MiniMaxSpeechSynthesizer::new(
            endpoint,
            raw.model.clone(),
            voice,
            raw.credential.clone(),
            timeout,
        )
        .ok()
        .map(|p| Arc::new(p) as Arc<dyn TtsProvider>),
        SpeechProviderKind::Groq => None,
    }
}

fn parse_provider_kind(raw: &str) -> Option<SpeechProviderKind> {
    match raw {
        "dashscope" => Some(SpeechProviderKind::Dashscope),
        "siliconflow" => Some(SpeechProviderKind::Siliconflow),
        "zhipu" => Some(SpeechProviderKind::Zhipu),
        "minimax" => Some(SpeechProviderKind::Minimax),
        "openai" => Some(SpeechProviderKind::Openai),
        "groq" => Some(SpeechProviderKind::Groq),
        _ => None,
    }
}

const fn provider_str(kind: SpeechProviderKind) -> &'static str {
    match kind {
        SpeechProviderKind::Dashscope => "dashscope",
        SpeechProviderKind::Siliconflow => "siliconflow",
        SpeechProviderKind::Zhipu => "zhipu",
        SpeechProviderKind::Minimax => "minimax",
        SpeechProviderKind::Openai => "openai",
        SpeechProviderKind::Groq => "groq",
    }
}

const fn default_endpoint_for(kind: SpeechProviderKind) -> &'static str {
    match kind {
        SpeechProviderKind::Dashscope => DEFAULT_DASHSCOPE_ENDPOINT,
        SpeechProviderKind::Siliconflow => DEFAULT_SILICONFLOW_ENDPOINT,
        SpeechProviderKind::Zhipu => DEFAULT_ZHIPU_ENDPOINT,
        SpeechProviderKind::Minimax => DEFAULT_MINIMAX_ENDPOINT,
        SpeechProviderKind::Openai => DEFAULT_OPENAI_ENDPOINT,
        SpeechProviderKind::Groq => DEFAULT_GROQ_ENDPOINT,
    }
}

fn normalize_endpoint(
    raw: &str,
    provider: SpeechProviderKind,
) -> Result<String, SpeechConfigError> {
    let trimmed = raw.trim();
    let candidate = if trimmed.is_empty() {
        default_endpoint_for(provider)
    } else {
        trimmed
    };
    if candidate.len() > MAX_TEXT_BYTES || !valid_endpoint(candidate) {
        return Err(SpeechConfigError::Invalid(
            "语音服务端点必须是 HTTPS 地址，或本机 127.0.0.1 / localhost HTTP 地址。",
        ));
    }
    Ok(candidate.to_owned())
}

fn normalize_required_text(raw: &str, message: &'static str) -> Result<String, SpeechConfigError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_TEXT_BYTES || trimmed.contains(['\r', '\n']) {
        return Err(SpeechConfigError::Invalid(message));
    }
    Ok(trimmed.to_owned())
}

fn resolve_next_credential(
    supplied: Option<&str>,
    clear_credential: bool,
    existing: Option<&str>,
    missing_message: &'static str,
) -> Result<String, SpeechConfigError> {
    let trimmed_supplied = supplied.map(str::trim).filter(|s| !s.is_empty());
    if let Some(secret) = trimmed_supplied {
        if secret.len() > MAX_TEXT_BYTES || secret.contains(['\r', '\n']) {
            return Err(SpeechConfigError::Invalid("API Key 凭据格式无效或过长。"));
        }
        return Ok(secret.to_owned());
    }
    if clear_credential {
        return Err(SpeechConfigError::Invalid(missing_message));
    }
    let Some(prev) = existing.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(SpeechConfigError::Invalid(missing_message));
    };
    Ok(prev.to_owned())
}

fn resolve_next_tts_credential(
    supplied: Option<&str>,
    clear_credential: bool,
    reuse_asr_credential: bool,
    existing_tts: Option<&str>,
    asr_credential: Option<&str>,
) -> Result<String, SpeechConfigError> {
    let trimmed_supplied = supplied.map(str::trim).filter(|s| !s.is_empty());
    if let Some(secret) = trimmed_supplied {
        if secret.len() > MAX_TEXT_BYTES || secret.contains(['\r', '\n']) {
            return Err(SpeechConfigError::Invalid("API Key 凭据格式无效或过长。"));
        }
        return Ok(secret.to_owned());
    }
    if clear_credential {
        return Err(SpeechConfigError::Invalid(
            "启用语音播放（TTS）时必须填写或保留 API Key 凭据。",
        ));
    }
    if reuse_asr_credential {
        let Some(reused) = asr_credential.map(str::trim).filter(|s| !s.is_empty()) else {
            return Err(SpeechConfigError::Invalid(
                "启用复用语音识别 API Key 时，请先在语音识别中填写或保存 API Key。",
            ));
        };
        if reused.len() > MAX_TEXT_BYTES || reused.contains(['\r', '\n']) {
            return Err(SpeechConfigError::Invalid("API Key 凭据格式无效或过长。"));
        }
        return Ok(reused.to_owned());
    }
    let Some(prev) = existing_tts.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(SpeechConfigError::Invalid(
            "请填写或保留语音播放（TTS）API Key 凭据。",
        ));
    };
    Ok(prev.to_owned())
}

fn mask_secret(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 8 {
        return Some("****".to_owned());
    }
    let prefix: String = chars[..3].iter().collect();
    let suffix: String = chars[chars.len() - 4..].iter().collect();
    Some(format!("{prefix}****{suffix}"))
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.trim() == value
}

fn valid_timeout(value: u64) -> bool {
    value > 0 && value <= MAX_TIMEOUT_MS
}

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

    #[tokio::test]
    async fn update_and_read_configuration_preserves_other_toml_sections_and_existing_secrets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# keep comment\nschema_version = 1\ndefault_model = \"deepseek\"\n\n[models.deepseek]\nmodel = \"deepseek-chat\"\n",
        )
        .expect("seed config");
        let source = crate::config_source::LocalConfigSource::new(path.clone());

        let initial = read_configuration(&source, DeviceSpeechServicesSnapshot::default())
            .await
            .expect("read initial");
        assert!(!initial.asr.enabled);
        assert!(!initial.tts.enabled);
        assert!(initial.revision.is_some());

        let first = update_configuration(
            &source,
            UpdateSpeechConfigurationRequest {
                expected_revision: initial.revision.clone(),
                asr: UpdateAsrConfigurationInput {
                    enabled: true,
                    provider: SpeechProviderKind::Openai,
                    endpoint: "https://api.openai.com/v1".to_owned(),
                    model: "whisper-1".to_owned(),
                    timeout_ms: 25_000,
                    credential: Some(assistant_protocol::SecretValue::new(
                        "sk-initial-asr-secret-9876".to_owned(),
                    )),
                    clear_credential: false,
                },
                tts: UpdateTtsConfigurationInput {
                    enabled: true,
                    provider: SpeechProviderKind::Dashscope,
                    endpoint: "https://dashscope.aliyuncs.com".to_owned(),
                    model: "cosyvoice-v2".to_owned(),
                    voice: "longxiaochun_v2".to_owned(),
                    timeout_ms: 30_000,
                    credential: Some(assistant_protocol::SecretValue::new(
                        "sk-tts-123456".to_owned(),
                    )),
                    clear_credential: false,
                    reuse_asr_credential: false,
                },
            },
        )
        .await
        .expect("first update");
        assert_eq!(
            first.compiled.status.asr,
            SpeechServiceStatusSnapshot::Ready
        );
        assert_eq!(
            first.compiled.status.tts,
            SpeechServiceStatusSnapshot::Ready
        );
        assert_eq!(
            first.configuration.asr.credential_masked.as_deref(),
            Some("sk-****9876")
        );

        // 留空凭据更新其他字段，必须保留已有密钥且保留原有模型配置。
        let second = update_configuration(
            &source,
            UpdateSpeechConfigurationRequest {
                expected_revision: first.configuration.revision.clone(),
                asr: UpdateAsrConfigurationInput {
                    enabled: true,
                    provider: SpeechProviderKind::Openai,
                    endpoint: "https://api.openai.com/v1".to_owned(),
                    model: "gpt-4o-mini-transcribe".to_owned(),
                    timeout_ms: 20_000,
                    credential: None,
                    clear_credential: false,
                },
                tts: UpdateTtsConfigurationInput {
                    enabled: false,
                    provider: SpeechProviderKind::Dashscope,
                    endpoint: String::new(),
                    model: String::new(),
                    voice: String::new(),
                    timeout_ms: 30_000,
                    credential: None,
                    clear_credential: false,
                    reuse_asr_credential: false,
                },
            },
        )
        .await
        .expect("second update");
        assert!(second.compiled.asr.is_some());
        assert!(second.compiled.tts.is_none());
        assert_eq!(
            second.configuration.asr.credential_masked.as_deref(),
            Some("sk-****9876")
        );

        let contents = std::fs::read_to_string(&path).expect("read written config");
        assert!(contents.contains("# keep comment"));
        assert!(contents.contains("[models.deepseek]"));
        assert!(!contents.contains("[speech.tts]"));

        // 过期 revision 必须触发冲突。
        let conflict = update_configuration(
            &source,
            UpdateSpeechConfigurationRequest {
                expected_revision: initial.revision,
                asr: UpdateAsrConfigurationInput {
                    enabled: false,
                    provider: SpeechProviderKind::Dashscope,
                    endpoint: String::new(),
                    model: String::new(),
                    timeout_ms: 30_000,
                    credential: None,
                    clear_credential: false,
                },
                tts: UpdateTtsConfigurationInput {
                    enabled: false,
                    provider: SpeechProviderKind::Dashscope,
                    endpoint: String::new(),
                    model: String::new(),
                    voice: String::new(),
                    timeout_ms: 30_000,
                    credential: None,
                    clear_credential: false,
                    reuse_asr_credential: false,
                },
            },
        )
        .await;
        assert!(matches!(conflict, Err(SpeechConfigError::Conflict)));

        // reuse_asr_credential 允许从已保存 ASR 凭据直接开通同厂商 TTS，且不支持组合会明确报错。
        let reused = update_configuration(
            &source,
            UpdateSpeechConfigurationRequest {
                expected_revision: second.configuration.revision.clone(),
                asr: UpdateAsrConfigurationInput {
                    enabled: true,
                    provider: SpeechProviderKind::Siliconflow,
                    endpoint: "https://api.siliconflow.cn/v1".to_owned(),
                    model: "FunAudioLLM/SenseVoiceSmall".to_owned(),
                    timeout_ms: 30_000,
                    credential: Some(assistant_protocol::SecretValue::new(
                        "sk-sf-asr-1234".to_owned(),
                    )),
                    clear_credential: false,
                },
                tts: UpdateTtsConfigurationInput {
                    enabled: true,
                    provider: SpeechProviderKind::Siliconflow,
                    endpoint: String::new(),
                    model: "FunAudioLLM/CosyVoice2-0.5B".to_owned(),
                    voice: "claire".to_owned(),
                    timeout_ms: 30_000,
                    credential: None,
                    clear_credential: false,
                    reuse_asr_credential: true,
                },
            },
        )
        .await
        .expect("reuse asr secret");
        assert_eq!(
            reused.configuration.tts.endpoint,
            DEFAULT_SILICONFLOW_ENDPOINT
        );
        assert_eq!(reused.configuration.tts.voice, "claire");
        assert_eq!(
            reused.configuration.tts.credential_masked.as_deref(),
            Some("sk-****1234")
        );
        assert_eq!(
            reused.compiled.status.tts,
            SpeechServiceStatusSnapshot::Ready
        );

        let minimax_asr_err = compile_asr_draft(
            &source,
            &UpdateAsrConfigurationInput {
                enabled: false,
                provider: SpeechProviderKind::Minimax,
                endpoint: String::new(),
                model: "whisper-1".to_owned(),
                timeout_ms: 30_000,
                credential: Some(assistant_protocol::SecretValue::new("sk-mm".to_owned())),
                clear_credential: false,
            },
        )
        .await;
        assert!(matches!(
            minimax_asr_err,
            Err(SpeechConfigError::Invalid(_))
        ));
    }
}
