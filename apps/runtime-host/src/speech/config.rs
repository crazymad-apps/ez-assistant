//! 从 Runtime Home 的同一份安全配置文档编译、读取与 CAS 更新 Host 私有语音能力。

use std::{path::PathBuf, sync::Arc, time::Duration};

use assistant_protocol::{
    AsrConfigurationSnapshot, AsrProfileSnapshot, DeleteSpeechProfileRequest,
    DeviceSpeechServicesSnapshot, RuntimeErrorCode, RuntimeErrorInfo, SaveAsrProfileRequest,
    SaveTtsProfileRequest, SetActiveSpeechProfileRequest, SpeechCapabilityKind,
    SpeechConfigurationSnapshot, SpeechProviderKind, SpeechServiceStatusSnapshot,
    TtsConfigurationSnapshot, TtsProfileSnapshot, UpdateAsrConfigurationInput,
    UpdateSpeechConfigurationRequest, UpdateTtsConfigurationInput,
};
use assistant_runtime::{ConfigSourceLoad, ConfigSourceReplace, RuntimeConfigSource};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use super::provider::{
    AsrProvider, DashScopeSpeechRecognizer, DashScopeSpeechSynthesizer, DeepSeekSpeechRecognizer,
    DeepSeekSpeechSynthesizer, MiniMaxSpeechSynthesizer, OpenAiSpeechRecognizer,
    OpenAiSpeechSynthesizer, TtsProvider,
};

const DEFAULT_DASHSCOPE_ENDPOINT: &str = "https://dashscope.aliyuncs.com";
const DEFAULT_DEEPSEEK_ENDPOINT: &str = "https://api.deepseek.com/v1";
const DEFAULT_SILICONFLOW_ENDPOINT: &str = "https://api.siliconflow.cn/v1";
const DEFAULT_ZHIPU_ENDPOINT: &str = "https://open.bigmodel.cn/api/paas/v4";
const DEFAULT_MINIMAX_ENDPOINT: &str = "https://api.minimaxi.com/v1/t2a_v2";
const DEFAULT_OPENAI_ENDPOINT: &str = "https://api.openai.com/v1";
const DEFAULT_GROQ_ENDPOINT: &str = "https://api.groq.com/openai/v1";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 120_000;
const MAX_TEXT_BYTES: usize = 512;
const MAX_PROFILE_NAME_BYTES: usize = 128;
const MAX_SPEECH_PROFILES: usize = 32;
const LEGACY_ASR_PROFILE_ID: &str = "asr-default";
const LEGACY_TTS_PROFILE_ID: &str = "tts-default";

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
    active_asr_id: Option<String>,
    active_tts_id: Option<String>,
    asr_profiles: Option<Vec<toml::Value>>,
    tts_profiles: Option<Vec<toml::Value>>,
    debug_audio_directory: Option<PathBuf>,
}

/// `[speech.asr]` 的旧版单槽 Provider 配置（向后兼容自动迁移）。
#[derive(Clone, Deserialize, Serialize)]
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

/// `[speech.tts]` 的旧版单槽 Provider 配置（向后兼容自动迁移）。
#[derive(Clone, Deserialize, Serialize)]
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

/// `[[speech.asr_profiles]]` 已保存 ASR 配置条目。
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawAsrProfile {
    id: String,
    name: String,
    provider: String,
    model: String,
    credential: String,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

impl RawAsrProfile {
    fn to_raw_config(&self) -> RawAsrConfig {
        RawAsrConfig {
            provider: self.provider.clone(),
            model: self.model.clone(),
            credential: self.credential.clone(),
            endpoint: self.endpoint.clone(),
            timeout_ms: self.timeout_ms,
        }
    }
}

/// `[[speech.tts_profiles]]` 已保存 TTS 配置条目。
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawTtsProfile {
    id: String,
    name: String,
    provider: String,
    model: String,
    voice: String,
    credential: String,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

impl RawTtsProfile {
    fn to_raw_config(&self) -> RawTtsConfig {
        RawTtsConfig {
            provider: self.provider.clone(),
            model: self.model.clone(),
            voice: self.voice.clone(),
            credential: self.credential.clone(),
            endpoint: self.endpoint.clone(),
            timeout_ms: self.timeout_ms,
        }
    }
}

#[derive(Default)]
struct ResolvedSpeechState {
    asr_profiles: Vec<RawAsrProfile>,
    active_asr_id: Option<String>,
    tts_profiles: Vec<RawTtsProfile>,
    active_tts_id: Option<String>,
    debug_audio_directory: Option<PathBuf>,
}

impl ResolvedSpeechState {
    fn active_asr(&self) -> Option<&RawAsrProfile> {
        let id = self.active_asr_id.as_deref()?;
        self.find_asr(id)
    }

    fn active_tts(&self) -> Option<&RawTtsProfile> {
        let id = self.active_tts_id.as_deref()?;
        self.find_tts(id)
    }

    fn find_asr(&self, id: &str) -> Option<&RawAsrProfile> {
        self.asr_profiles.iter().find(|profile| profile.id == id)
    }

    fn find_tts(&self, id: &str) -> Option<&RawTtsProfile> {
        self.tts_profiles.iter().find(|profile| profile.id == id)
    }
}

fn resolve_speech_state(speech: Option<RawSpeechConfig>) -> ResolvedSpeechState {
    let Some(speech) = speech else {
        return ResolvedSpeechState::default();
    };

    let (asr_profiles, active_asr_id) = match speech.asr_profiles {
        Some(raw_list) => {
            let mut profiles: Vec<RawAsrProfile> = Vec::new();
            for item in raw_list {
                let Ok(profile) = item.try_into::<RawAsrProfile>() else {
                    continue;
                };
                if profile.id.trim().is_empty()
                    || profiles.iter().any(|existing| existing.id == profile.id)
                {
                    continue;
                }
                profiles.push(profile);
            }
            let active = speech
                .active_asr_id
                .filter(|id| profiles.iter().any(|profile| profile.id == *id));
            (profiles, active)
        }
        None => match speech
            .asr
            .and_then(|raw| raw.try_into::<RawAsrConfig>().ok())
        {
            Some(legacy) => {
                let provider = parse_provider_kind(&legacy.provider)
                    .unwrap_or(SpeechProviderKind::Dashscope);
                let profile = RawAsrProfile {
                    id: LEGACY_ASR_PROFILE_ID.to_owned(),
                    name: default_asr_profile_name(provider, &legacy.model),
                    provider: legacy.provider,
                    model: legacy.model,
                    credential: legacy.credential,
                    endpoint: legacy.endpoint,
                    timeout_ms: legacy.timeout_ms,
                };
                (vec![profile], Some(LEGACY_ASR_PROFILE_ID.to_owned()))
            }
            None => (Vec::new(), None),
        },
    };

    let (tts_profiles, active_tts_id) = match speech.tts_profiles {
        Some(raw_list) => {
            let mut profiles: Vec<RawTtsProfile> = Vec::new();
            for item in raw_list {
                let Ok(profile) = item.try_into::<RawTtsProfile>() else {
                    continue;
                };
                if profile.id.trim().is_empty()
                    || profiles.iter().any(|existing| existing.id == profile.id)
                {
                    continue;
                }
                profiles.push(profile);
            }
            let active = speech
                .active_tts_id
                .filter(|id| profiles.iter().any(|profile| profile.id == *id));
            (profiles, active)
        }
        None => match speech
            .tts
            .and_then(|raw| raw.try_into::<RawTtsConfig>().ok())
        {
            Some(legacy) => {
                let provider = parse_provider_kind(&legacy.provider)
                    .unwrap_or(SpeechProviderKind::Dashscope);
                let profile = RawTtsProfile {
                    id: LEGACY_TTS_PROFILE_ID.to_owned(),
                    name: default_tts_profile_name(provider, &legacy.voice, &legacy.model),
                    provider: legacy.provider,
                    model: legacy.model,
                    voice: legacy.voice,
                    credential: legacy.credential,
                    endpoint: legacy.endpoint,
                    timeout_ms: legacy.timeout_ms,
                };
                (vec![profile], Some(LEGACY_TTS_PROFILE_ID.to_owned()))
            }
            None => (Vec::new(), None),
        },
    };

    ResolvedSpeechState {
        asr_profiles,
        active_asr_id,
        tts_profiles,
        active_tts_id,
        debug_audio_directory: speech.debug_audio_directory,
    }
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
    let state = resolve_speech_state(document.speech);
    let active_asr = state.active_asr().map(RawAsrProfile::to_raw_config);
    let active_tts = state.active_tts().map(RawTtsProfile::to_raw_config);
    let asr_timeout = Duration::from_millis(
        active_asr
            .as_ref()
            .map_or(DEFAULT_TIMEOUT_MS, |raw| raw.timeout_ms),
    );
    let tts_timeout = Duration::from_millis(
        active_tts
            .as_ref()
            .map_or(DEFAULT_TIMEOUT_MS, |raw| raw.timeout_ms),
    );
    let asr = active_asr.and_then(compile_asr);
    let tts = active_tts.and_then(compile_tts);
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
        debug_audio_directory: debug_audio_directory(state.debug_audio_directory),
        asr_timeout,
        tts_timeout,
    }
}

pub(super) async fn read_configuration(
    source: &dyn RuntimeConfigSource,
    status: DeviceSpeechServicesSnapshot,
) -> Result<SpeechConfigurationSnapshot, SpeechConfigError> {
    match source.load().await {
        ConfigSourceLoad::Missing => Ok(project_configuration(
            None,
            &ResolvedSpeechState::default(),
            status,
        )),
        ConfigSourceLoad::Document(doc) => {
            let revision = doc.revision().to_owned();
            let parsed = toml::from_str::<SpeechDocument>(doc.contents()).ok();
            let state = resolve_speech_state(parsed.and_then(|d| d.speech));
            Ok(project_configuration(Some(revision), &state, status))
        }
        ConfigSourceLoad::Unavailable(_) => Err(SpeechConfigError::Unavailable),
    }
}

async fn load_editable_document(
    source: &dyn RuntimeConfigSource,
    expected_revision: Option<&str>,
) -> Result<(DocumentMut, Option<String>, ResolvedSpeechState), SpeechConfigError> {
    let (document, current_revision) = match source.load().await {
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

    if expected_revision != current_revision.as_deref() {
        return Err(SpeechConfigError::Conflict);
    }

    let existing_parsed = toml::from_str::<SpeechDocument>(&document.to_string()).ok();
    let state = resolve_speech_state(existing_parsed.and_then(|d| d.speech));
    Ok((document, current_revision, state))
}

async fn persist_speech_state(
    source: &dyn RuntimeConfigSource,
    mut document: DocumentMut,
    current_revision: Option<String>,
    state: &ResolvedSpeechState,
) -> Result<SpeechConfigUpdateOutcome, SpeechConfigError> {
    write_speech_state(&mut document, state)?;
    let next_text = document.to_string();
    match source.replace(current_revision, next_text).await {
        ConfigSourceReplace::Applied(saved_doc) => {
            let compiled = compile_document_contents(saved_doc.contents());
            let configuration = project_configuration(
                Some(saved_doc.revision().to_owned()),
                state,
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

fn write_speech_state(
    document: &mut DocumentMut,
    state: &ResolvedSpeechState,
) -> Result<(), SpeechConfigError> {
    if state.asr_profiles.is_empty() && state.tts_profiles.is_empty() {
        if let Some(speech_table) = document.get_mut("speech").and_then(Item::as_table_mut) {
            speech_table.remove("asr");
            speech_table.remove("tts");
            speech_table.remove("active_asr_id");
            speech_table.remove("active_tts_id");
            speech_table.remove("asr_profiles");
            speech_table.remove("tts_profiles");
            if speech_table.is_empty() {
                document.remove("speech");
            }
        }
        return Ok(());
    }

    if !document.contains_key("speech") {
        let mut tbl = Table::new();
        tbl.set_implicit(false);
        document["speech"] = Item::Table(tbl);
    }
    let speech_table = document
        .get_mut("speech")
        .and_then(Item::as_table_mut)
        .ok_or(SpeechConfigError::Invalid(
            "[speech] 配置节点必须是 TOML 表。",
        ))?;

    // 统一迁移到多配置列表，移除旧版单槽表避免密钥重复残留。
    speech_table.remove("asr");
    speech_table.remove("tts");

    match &state.active_asr_id {
        Some(id) => {
            speech_table["active_asr_id"] = value(id.as_str());
        }
        None => {
            speech_table.remove("active_asr_id");
        }
    }
    match &state.active_tts_id {
        Some(id) => {
            speech_table["active_tts_id"] = value(id.as_str());
        }
        None => {
            speech_table.remove("active_tts_id");
        }
    }

    if state.asr_profiles.is_empty() {
        speech_table.remove("asr_profiles");
    } else {
        let mut array = ArrayOfTables::new();
        for profile in &state.asr_profiles {
            let section = toml::to_string(profile)
                .map_err(|_| SpeechConfigError::Unavailable)?
                .parse::<DocumentMut>()
                .map_err(|_| SpeechConfigError::Unavailable)?;
            array.push(section.as_table().clone());
        }
        speech_table["asr_profiles"] = Item::ArrayOfTables(array);
    }

    if state.tts_profiles.is_empty() {
        speech_table.remove("tts_profiles");
    } else {
        let mut array = ArrayOfTables::new();
        for profile in &state.tts_profiles {
            let section = toml::to_string(profile)
                .map_err(|_| SpeechConfigError::Unavailable)?
                .parse::<DocumentMut>()
                .map_err(|_| SpeechConfigError::Unavailable)?;
            array.push(section.as_table().clone());
        }
        speech_table["tts_profiles"] = Item::ArrayOfTables(array);
    }

    let has_scalar_keys = state.active_asr_id.is_some()
        || state.active_tts_id.is_some()
        || speech_table.contains_key("debug_audio_directory");
    speech_table.set_implicit(!has_scalar_keys);
    Ok(())
}

pub(super) async fn update_configuration(
    source: &dyn RuntimeConfigSource,
    request: UpdateSpeechConfigurationRequest,
) -> Result<SpeechConfigUpdateOutcome, SpeechConfigError> {
    let (document, current_revision, mut state) =
        load_editable_document(source, request.expected_revision.as_deref()).await?;

    let existing_asr = state.active_asr().map(RawAsrProfile::to_raw_config);
    let existing_tts = state.active_tts().map(RawTtsProfile::to_raw_config);

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

    match next_asr {
        Some(raw) => {
            let provider = parse_provider_kind(&raw.provider).unwrap_or(SpeechProviderKind::Dashscope);
            let default_name = default_asr_profile_name(provider, &raw.model);
            if let Some(active_id) = state.active_asr_id.clone()
                && let Some(profile) = state.asr_profiles.iter_mut().find(|p| p.id == active_id)
            {
                profile.provider = raw.provider;
                profile.model = raw.model;
                profile.credential = raw.credential;
                profile.endpoint = raw.endpoint;
                profile.timeout_ms = raw.timeout_ms;
                if profile.name.trim().is_empty() {
                    profile.name = default_name;
                }
            } else {
                let id = if state.find_asr(LEGACY_ASR_PROFILE_ID).is_none() {
                    LEGACY_ASR_PROFILE_ID.to_owned()
                } else {
                    generate_profile_id("asr", |candidate| state.find_asr(candidate).is_some())?
                };
                state.asr_profiles.push(RawAsrProfile {
                    id: id.clone(),
                    name: default_name,
                    provider: raw.provider,
                    model: raw.model,
                    credential: raw.credential,
                    endpoint: raw.endpoint,
                    timeout_ms: raw.timeout_ms,
                });
                state.active_asr_id = Some(id);
            }
        }
        None => {
            state.active_asr_id = None;
        }
    }

    match next_tts {
        Some(raw) => {
            let provider = parse_provider_kind(&raw.provider).unwrap_or(SpeechProviderKind::Dashscope);
            let default_name = default_tts_profile_name(provider, &raw.voice, &raw.model);
            if let Some(active_id) = state.active_tts_id.clone()
                && let Some(profile) = state.tts_profiles.iter_mut().find(|p| p.id == active_id)
            {
                profile.provider = raw.provider;
                profile.model = raw.model;
                profile.voice = raw.voice;
                profile.credential = raw.credential;
                profile.endpoint = raw.endpoint;
                profile.timeout_ms = raw.timeout_ms;
                if profile.name.trim().is_empty() {
                    profile.name = default_name;
                }
            } else {
                let id = if state.find_tts(LEGACY_TTS_PROFILE_ID).is_none() {
                    LEGACY_TTS_PROFILE_ID.to_owned()
                } else {
                    generate_profile_id("tts", |candidate| state.find_tts(candidate).is_some())?
                };
                state.tts_profiles.push(RawTtsProfile {
                    id: id.clone(),
                    name: default_name,
                    provider: raw.provider,
                    model: raw.model,
                    voice: raw.voice,
                    credential: raw.credential,
                    endpoint: raw.endpoint,
                    timeout_ms: raw.timeout_ms,
                });
                state.active_tts_id = Some(id);
            }
        }
        None => {
            state.active_tts_id = None;
        }
    }

    persist_speech_state(source, document, current_revision, &state).await
}

pub(super) async fn save_asr_profile(
    source: &dyn RuntimeConfigSource,
    request: SaveAsrProfileRequest,
) -> Result<SpeechConfigUpdateOutcome, SpeechConfigError> {
    let (document, current_revision, mut state) =
        load_editable_document(source, request.expected_revision.as_deref()).await?;

    let input = UpdateAsrConfigurationInput {
        enabled: true,
        provider: request.provider,
        endpoint: request.endpoint,
        model: request.model,
        timeout_ms: request.timeout_ms,
        credential: request.credential,
        clear_credential: request.clear_credential,
    };

    let target_id = if let Some(raw_id) = request.id {
        let id = raw_id.trim().to_owned();
        let Some(pos) = state.asr_profiles.iter().position(|p| p.id == id) else {
            return Err(SpeechConfigError::Invalid(
                "未找到要编辑的语音识别（ASR）配置，请刷新后重试。",
            ));
        };
        let existing = state.asr_profiles[pos].to_raw_config();
        let built = validate_and_build_asr(&input, Some(&existing))?;
        let name = normalize_profile_name(
            &request.name,
            || default_asr_profile_name(request.provider, &built.model),
        )?;
        state.asr_profiles[pos] = RawAsrProfile {
            id: id.clone(),
            name,
            provider: built.provider,
            model: built.model,
            credential: built.credential,
            endpoint: built.endpoint,
            timeout_ms: built.timeout_ms,
        };
        id
    } else {
        if state.asr_profiles.len() >= MAX_SPEECH_PROFILES {
            return Err(SpeechConfigError::Invalid(
                "已保存的语音识别（ASR）配置数量已达上限。",
            ));
        }
        let built = validate_and_build_asr(&input, None)?;
        let id = generate_profile_id("asr", |candidate| state.find_asr(candidate).is_some())?;
        let name = normalize_profile_name(
            &request.name,
            || default_asr_profile_name(request.provider, &built.model),
        )?;
        state.asr_profiles.push(RawAsrProfile {
            id: id.clone(),
            name,
            provider: built.provider,
            model: built.model,
            credential: built.credential,
            endpoint: built.endpoint,
            timeout_ms: built.timeout_ms,
        });
        id
    };

    if request.activate {
        state.active_asr_id = Some(target_id);
    }

    persist_speech_state(source, document, current_revision, &state).await
}

pub(super) async fn save_tts_profile(
    source: &dyn RuntimeConfigSource,
    request: SaveTtsProfileRequest,
) -> Result<SpeechConfigUpdateOutcome, SpeechConfigError> {
    let (document, current_revision, mut state) =
        load_editable_document(source, request.expected_revision.as_deref()).await?;

    let reuse_asr_secret = match request
        .reuse_asr_profile_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(asr_id) => {
            let Some(profile) = state.find_asr(asr_id) else {
                return Err(SpeechConfigError::Invalid(
                    "所选语音识别（ASR）配置不存在，无法复用其 API Key。",
                ));
            };
            Some(profile.credential.clone())
        }
        None => None,
    };

    let input = UpdateTtsConfigurationInput {
        enabled: true,
        provider: request.provider,
        endpoint: request.endpoint,
        model: request.model,
        voice: request.voice,
        timeout_ms: request.timeout_ms,
        credential: request.credential,
        clear_credential: request.clear_credential,
        reuse_asr_credential: reuse_asr_secret.is_some(),
    };

    let target_id = if let Some(raw_id) = request.id {
        let id = raw_id.trim().to_owned();
        let Some(pos) = state.tts_profiles.iter().position(|p| p.id == id) else {
            return Err(SpeechConfigError::Invalid(
                "未找到要编辑的语音合成（TTS）配置，请刷新后重试。",
            ));
        };
        let existing = state.tts_profiles[pos].to_raw_config();
        let built = validate_and_build_tts(&input, Some(&existing), reuse_asr_secret.as_deref())?;
        let name = normalize_profile_name(
            &request.name,
            || default_tts_profile_name(request.provider, &built.voice, &built.model),
        )?;
        state.tts_profiles[pos] = RawTtsProfile {
            id: id.clone(),
            name,
            provider: built.provider,
            model: built.model,
            voice: built.voice,
            credential: built.credential,
            endpoint: built.endpoint,
            timeout_ms: built.timeout_ms,
        };
        id
    } else {
        if state.tts_profiles.len() >= MAX_SPEECH_PROFILES {
            return Err(SpeechConfigError::Invalid(
                "已保存的语音合成（TTS）配置数量已达上限。",
            ));
        }
        let built = validate_and_build_tts(&input, None, reuse_asr_secret.as_deref())?;
        let id = generate_profile_id("tts", |candidate| state.find_tts(candidate).is_some())?;
        let name = normalize_profile_name(
            &request.name,
            || default_tts_profile_name(request.provider, &built.voice, &built.model),
        )?;
        state.tts_profiles.push(RawTtsProfile {
            id: id.clone(),
            name,
            provider: built.provider,
            model: built.model,
            voice: built.voice,
            credential: built.credential,
            endpoint: built.endpoint,
            timeout_ms: built.timeout_ms,
        });
        id
    };

    if request.activate {
        state.active_tts_id = Some(target_id);
    }

    persist_speech_state(source, document, current_revision, &state).await
}

pub(super) async fn set_active_profile(
    source: &dyn RuntimeConfigSource,
    request: SetActiveSpeechProfileRequest,
) -> Result<SpeechConfigUpdateOutcome, SpeechConfigError> {
    let (document, current_revision, mut state) =
        load_editable_document(source, request.expected_revision.as_deref()).await?;

    let target_id = request
        .profile_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    match request.capability {
        SpeechCapabilityKind::Asr => {
            if let Some(id) = &target_id
                && state.find_asr(id).is_none()
            {
                return Err(SpeechConfigError::Invalid(
                    "未找到要启用的语音识别（ASR）配置。",
                ));
            }
            state.active_asr_id = target_id;
        }
        SpeechCapabilityKind::Tts => {
            if let Some(id) = &target_id
                && state.find_tts(id).is_none()
            {
                return Err(SpeechConfigError::Invalid(
                    "未找到要启用的语音合成（TTS）配置。",
                ));
            }
            state.active_tts_id = target_id;
        }
    }

    persist_speech_state(source, document, current_revision, &state).await
}

pub(super) async fn delete_profile(
    source: &dyn RuntimeConfigSource,
    request: DeleteSpeechProfileRequest,
) -> Result<SpeechConfigUpdateOutcome, SpeechConfigError> {
    let (document, current_revision, mut state) =
        load_editable_document(source, request.expected_revision.as_deref()).await?;

    let id = request.profile_id.trim();
    match request.capability {
        SpeechCapabilityKind::Asr => {
            let Some(pos) = state.asr_profiles.iter().position(|p| p.id == id) else {
                return Err(SpeechConfigError::Invalid(
                    "未找到要删除的语音识别（ASR）配置，请刷新后重试。",
                ));
            };
            state.asr_profiles.remove(pos);
            if state.active_asr_id.as_deref() == Some(id) {
                state.active_asr_id = None;
            }
        }
        SpeechCapabilityKind::Tts => {
            let Some(pos) = state.tts_profiles.iter().position(|p| p.id == id) else {
                return Err(SpeechConfigError::Invalid(
                    "未找到要删除的语音合成（TTS）配置，请刷新后重试。",
                ));
            };
            state.tts_profiles.remove(pos);
            if state.active_tts_id.as_deref() == Some(id) {
                state.active_tts_id = None;
            }
        }
    }

    persist_speech_state(source, document, current_revision, &state).await
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
            "MiniMax 暂仅支持语音播放（TTS），语音识别请选择阿里云百炼、DeepSeek、硅基流动、智谱、OpenAI 或 Groq。",
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
            "Groq 暂仅支持语音识别（ASR），语音播放请选择阿里云百炼、DeepSeek、硅基流动、智谱、MiniMax 或 OpenAI。",
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
    profile_id: Option<&str>,
) -> Result<(Arc<dyn AsrProvider>, Duration), SpeechConfigError> {
    let state = load_resolved_state(source).await;
    let existing_asr = match profile_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => state.find_asr(id).map(RawAsrProfile::to_raw_config),
        None => state.active_asr().map(RawAsrProfile::to_raw_config),
    };
    let raw = validate_and_build_asr(input, existing_asr.as_ref())?;
    let timeout = Duration::from_millis(raw.timeout_ms);
    let provider =
        compile_asr_ref(&raw).ok_or(SpeechConfigError::Invalid("语音识别（ASR）配置无效。"))?;
    Ok((provider, timeout))
}

pub(super) async fn compile_tts_draft(
    source: &dyn RuntimeConfigSource,
    input: &UpdateTtsConfigurationInput,
    profile_id: Option<&str>,
    reuse_asr_profile_id: Option<&str>,
    fallback_asr_credential: Option<&str>,
) -> Result<(Arc<dyn TtsProvider>, Duration), SpeechConfigError> {
    let state = load_resolved_state(source).await;
    let existing_tts = match profile_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => state.find_tts(id).map(RawTtsProfile::to_raw_config),
        None => state.active_tts().map(RawTtsProfile::to_raw_config),
    };
    let resolved_asr = fallback_asr_credential
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            reuse_asr_profile_id
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .and_then(|id| state.find_asr(id))
                .map(|profile| profile.credential.clone())
        })
        .or_else(|| state.active_asr().map(|profile| profile.credential.clone()));
    let raw = validate_and_build_tts(input, existing_tts.as_ref(), resolved_asr.as_deref())?;
    let timeout = Duration::from_millis(raw.timeout_ms);
    let provider =
        compile_tts_ref(&raw).ok_or(SpeechConfigError::Invalid("语音播放（TTS）配置无效。"))?;
    Ok((provider, timeout))
}

async fn load_resolved_state(source: &dyn RuntimeConfigSource) -> ResolvedSpeechState {
    let ConfigSourceLoad::Document(doc) = source.load().await else {
        return ResolvedSpeechState::default();
    };
    let parsed = toml::from_str::<SpeechDocument>(doc.contents()).ok();
    resolve_speech_state(parsed.and_then(|d| d.speech))
}

fn project_asr_profile(cfg: &RawAsrProfile) -> AsrProfileSnapshot {
    let provider = parse_provider_kind(&cfg.provider).unwrap_or(SpeechProviderKind::Dashscope);
    let default_ep = default_endpoint_for(provider);
    AsrProfileSnapshot {
        id: cfg.id.clone(),
        name: if cfg.name.trim().is_empty() {
            default_asr_profile_name(provider, &cfg.model)
        } else {
            cfg.name.clone()
        },
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

fn project_tts_profile(cfg: &RawTtsProfile) -> TtsProfileSnapshot {
    let provider = parse_provider_kind(&cfg.provider).unwrap_or(SpeechProviderKind::Dashscope);
    let default_ep = default_endpoint_for(provider);
    TtsProfileSnapshot {
        id: cfg.id.clone(),
        name: if cfg.name.trim().is_empty() {
            default_tts_profile_name(provider, &cfg.voice, &cfg.model)
        } else {
            cfg.name.clone()
        },
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

fn project_configuration(
    revision: Option<String>,
    state: &ResolvedSpeechState,
    status: DeviceSpeechServicesSnapshot,
) -> SpeechConfigurationSnapshot {
    let asr_profiles: Vec<AsrProfileSnapshot> = state
        .asr_profiles
        .iter()
        .map(project_asr_profile)
        .collect();
    let tts_profiles: Vec<TtsProfileSnapshot> = state
        .tts_profiles
        .iter()
        .map(project_tts_profile)
        .collect();

    let asr_snapshot = match state.active_asr().map(project_asr_profile) {
        Some(active) => AsrConfigurationSnapshot {
            enabled: true,
            provider: active.provider,
            endpoint: active.endpoint,
            model: active.model,
            timeout_ms: active.timeout_ms,
            credential_configured: active.credential_configured,
            credential_masked: active.credential_masked,
        },
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
    let tts_snapshot = match state.active_tts().map(project_tts_profile) {
        Some(active) => TtsConfigurationSnapshot {
            enabled: true,
            provider: active.provider,
            endpoint: active.endpoint,
            model: active.model,
            voice: active.voice,
            timeout_ms: active.timeout_ms,
            credential_configured: active.credential_configured,
            credential_masked: active.credential_masked,
        },
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
        asr_profiles,
        active_asr_id: state.active_asr_id.clone(),
        tts_profiles,
        active_tts_id: state.active_tts_id.clone(),
        status,
    }
}

fn generate_profile_id(
    prefix: &str,
    exists: impl Fn(&str) -> bool,
) -> Result<String, SpeechConfigError> {
    for _ in 0..8 {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).map_err(|_| SpeechConfigError::Unavailable)?;
        let suffix = u64::from_be_bytes(bytes);
        let candidate = format!("{prefix}-{suffix:016x}");
        if !exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(SpeechConfigError::Unavailable)
}

fn normalize_profile_name(
    raw: &str,
    fallback: impl FnOnce() -> String,
) -> Result<String, SpeechConfigError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(fallback());
    }
    if trimmed.len() > MAX_PROFILE_NAME_BYTES || trimmed.contains(['\r', '\n']) {
        return Err(SpeechConfigError::Invalid("配置名称过长或包含非法换行符。"));
    }
    Ok(trimmed.to_owned())
}

const fn provider_display_name(kind: SpeechProviderKind) -> &'static str {
    match kind {
        SpeechProviderKind::Dashscope => "阿里云百炼",
        SpeechProviderKind::Deepseek => "DeepSeek",
        SpeechProviderKind::Siliconflow => "硅基流动",
        SpeechProviderKind::Zhipu => "智谱 BigModel",
        SpeechProviderKind::Minimax => "MiniMax",
        SpeechProviderKind::Openai => "OpenAI",
        SpeechProviderKind::Groq => "Groq",
    }
}

fn default_asr_profile_name(provider: SpeechProviderKind, model: &str) -> String {
    let model = model.trim();
    if model.is_empty() {
        provider_display_name(provider).to_owned()
    } else {
        format!("{} · {model}", provider_display_name(provider))
    }
}

fn default_tts_profile_name(provider: SpeechProviderKind, voice: &str, model: &str) -> String {
    let label = if !voice.trim().is_empty() {
        voice.trim()
    } else {
        model.trim()
    };
    if label.is_empty() {
        provider_display_name(provider).to_owned()
    } else {
        format!("{} · {label}", provider_display_name(provider))
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
        SpeechProviderKind::Deepseek => {
            DeepSeekSpeechRecognizer::new(endpoint, raw.credential.clone(), timeout)
                .ok()
                .map(|p| Arc::new(p) as Arc<dyn AsrProvider>)
        }
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
        SpeechProviderKind::Deepseek => {
            DeepSeekSpeechSynthesizer::new(endpoint, voice, raw.credential.clone(), timeout)
                .ok()
                .map(|p| Arc::new(p) as Arc<dyn TtsProvider>)
        }
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
        "deepseek" => Some(SpeechProviderKind::Deepseek),
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
        SpeechProviderKind::Deepseek => "deepseek",
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
        SpeechProviderKind::Deepseek => DEFAULT_DEEPSEEK_ENDPOINT,
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
            None,
        )
        .await;
        assert!(matches!(
            minimax_asr_err,
            Err(SpeechConfigError::Invalid(_))
        ));

        let deepseek_saved = update_configuration(
            &source,
            UpdateSpeechConfigurationRequest {
                expected_revision: reused.configuration.revision,
                asr: UpdateAsrConfigurationInput {
                    enabled: true,
                    provider: SpeechProviderKind::Deepseek,
                    endpoint: String::new(),
                    model: "deepseek-asr".to_owned(),
                    timeout_ms: 30_000,
                    credential: Some(assistant_protocol::SecretValue::new(
                        "sk-deepseek-key".to_owned(),
                    )),
                    clear_credential: false,
                },
                tts: UpdateTtsConfigurationInput {
                    enabled: true,
                    provider: SpeechProviderKind::Deepseek,
                    endpoint: String::new(),
                    model: "deepseek-tts".to_owned(),
                    voice: "tide".to_owned(),
                    timeout_ms: 30_000,
                    credential: None,
                    clear_credential: false,
                    reuse_asr_credential: true,
                },
            },
        )
        .await
        .expect("save deepseek speech config");
        assert_eq!(
            deepseek_saved.configuration.asr.provider,
            SpeechProviderKind::Deepseek
        );
        assert_eq!(
            deepseek_saved.configuration.asr.endpoint,
            DEFAULT_DEEPSEEK_ENDPOINT
        );
        assert_eq!(
            deepseek_saved.configuration.tts.provider,
            SpeechProviderKind::Deepseek
        );
        assert_eq!(
            deepseek_saved.configuration.tts.endpoint,
            DEFAULT_DEEPSEEK_ENDPOINT
        );
        assert_eq!(deepseek_saved.configuration.tts.voice, "tide");
        assert_eq!(
            deepseek_saved.compiled.status.asr,
            SpeechServiceStatusSnapshot::Ready
        );
        assert_eq!(
            deepseek_saved.compiled.status.tts,
            SpeechServiceStatusSnapshot::Ready
        );
    }

    #[tokio::test]
    async fn speech_profiles_migrate_legacy_and_support_full_list_crud_lifecycle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
schema_version = 1

[speech.asr]
provider = "dashscope"
model = "qwen3-asr-flash"
credential = "sk-legacy-asr-1111"
endpoint = "https://dashscope.aliyuncs.com"
timeout_ms = 30000

[speech.tts]
provider = "dashscope"
model = "cosyvoice-v2"
voice = "longxiaochun_v2"
credential = "sk-legacy-tts-2222"
endpoint = "https://dashscope.aliyuncs.com"
timeout_ms = 30000
"#,
        )
        .expect("seed legacy config");
        let source = crate::config_source::LocalConfigSource::new(path.clone());

        // 1. Legacy config automatically projects into 1-item lists with active IDs
        let initial = read_configuration(&source, DeviceSpeechServicesSnapshot::default())
            .await
            .expect("read legacy");
        assert_eq!(initial.asr_profiles.len(), 1);
        assert_eq!(initial.tts_profiles.len(), 1);
        assert_eq!(initial.active_asr_id.as_deref(), Some(LEGACY_ASR_PROFILE_ID));
        assert_eq!(initial.active_tts_id.as_deref(), Some(LEGACY_TTS_PROFILE_ID));
        assert_eq!(
            initial.asr_profiles[0].credential_masked.as_deref(),
            Some("sk-****1111")
        );

        // 2. Add a second ASR profile (inactive) -> migrates legacy seamlessly without overwriting it
        let added_asr = save_asr_profile(
            &source,
            SaveAsrProfileRequest {
                expected_revision: initial.revision.clone(),
                id: None,
                name: "DeepSeek 备用识别".to_owned(),
                provider: SpeechProviderKind::Deepseek,
                endpoint: String::new(),
                model: "deepseek-asr".to_owned(),
                timeout_ms: 25_000,
                credential: Some(assistant_protocol::SecretValue::new(
                    "sk-deepseek-asr-3333".to_owned(),
                )),
                clear_credential: false,
                activate: false,
            },
        )
        .await
        .expect("add second asr");
        assert_eq!(added_asr.configuration.asr_profiles.len(), 2);
        assert_eq!(
            added_asr.configuration.active_asr_id.as_deref(),
            Some(LEGACY_ASR_PROFILE_ID)
        );
        let ds_asr_id = added_asr.configuration.asr_profiles[1].id.clone();
        assert_eq!(
            added_asr.configuration.asr_profiles[1].name,
            "DeepSeek 备用识别"
        );
        // Legacy [speech.asr] removed after migration
        let migrated_toml = std::fs::read_to_string(&path).expect("read migrated");
        assert!(!migrated_toml.contains("[speech.asr]"));
        assert!(migrated_toml.contains("[[speech.asr_profiles]]"));
        assert!(migrated_toml.contains("[[speech.tts_profiles]]"));

        // 3. Add a TTS profile reusing the second (inactive) ASR profile's API key and activate it
        let added_tts = save_tts_profile(
            &source,
            SaveTtsProfileRequest {
                expected_revision: added_asr.configuration.revision.clone(),
                id: None,
                name: "DeepSeek 男声".to_owned(),
                provider: SpeechProviderKind::Deepseek,
                endpoint: String::new(),
                model: "deepseek-tts".to_owned(),
                voice: "tide".to_owned(),
                timeout_ms: 30_000,
                credential: None,
                clear_credential: false,
                reuse_asr_profile_id: Some(ds_asr_id.clone()),
                activate: true,
            },
        )
        .await
        .expect("add second tts reusing ds asr key");
        assert_eq!(added_tts.configuration.tts_profiles.len(), 2);
        let ds_tts_id = added_tts.configuration.tts_profiles[1].id.clone();
        assert_eq!(
            added_tts.configuration.active_tts_id.as_deref(),
            Some(ds_tts_id.as_str())
        );
        assert_eq!(
            added_tts.configuration.tts_profiles[1]
                .credential_masked
                .as_deref(),
            Some("sk-****3333")
        );
        assert_eq!(added_tts.configuration.tts.provider, SpeechProviderKind::Deepseek);

        // 4. Edit existing ASR profile without re-sending credential -> preserves stored secret
        let edited_asr = save_asr_profile(
            &source,
            SaveAsrProfileRequest {
                expected_revision: added_tts.configuration.revision.clone(),
                id: Some(ds_asr_id.clone()),
                name: "DeepSeek 主力识别".to_owned(),
                provider: SpeechProviderKind::Deepseek,
                endpoint: DEFAULT_DEEPSEEK_ENDPOINT.to_owned(),
                model: "deepseek-asr".to_owned(),
                timeout_ms: 20_000,
                credential: None,
                clear_credential: false,
                activate: true,
            },
        )
        .await
        .expect("edit ds asr");
        assert_eq!(
            edited_asr.configuration.active_asr_id.as_deref(),
            Some(ds_asr_id.as_str())
        );
        assert_eq!(edited_asr.configuration.asr.provider, SpeechProviderKind::Deepseek);
        assert_eq!(
            edited_asr.configuration.asr.credential_masked.as_deref(),
            Some("sk-****3333")
        );

        // 5. Test inactive legacy ASR profile by profile_id without sending credential
        let compiled_inactive = compile_asr_draft(
            &source,
            &UpdateAsrConfigurationInput {
                enabled: false,
                provider: SpeechProviderKind::Dashscope,
                endpoint: DEFAULT_DASHSCOPE_ENDPOINT.to_owned(),
                model: "qwen3-asr-flash".to_owned(),
                timeout_ms: 30_000,
                credential: None,
                clear_credential: false,
            },
            Some(LEGACY_ASR_PROFILE_ID),
        )
        .await;
        assert!(compiled_inactive.is_ok());

        // 6. Disable ASR via set_active_profile(None) -> status Unavailable, profiles preserved!
        let disabled_asr = set_active_profile(
            &source,
            SetActiveSpeechProfileRequest {
                expected_revision: edited_asr.configuration.revision.clone(),
                capability: SpeechCapabilityKind::Asr,
                profile_id: None,
            },
        )
        .await
        .expect("disable asr");
        assert_eq!(disabled_asr.configuration.asr_profiles.len(), 2);
        assert_eq!(disabled_asr.configuration.active_asr_id, None);
        assert!(!disabled_asr.configuration.asr.enabled);
        assert_eq!(
            disabled_asr.compiled.status.asr,
            SpeechServiceStatusSnapshot::Unavailable
        );
        assert_eq!(
            disabled_asr.compiled.status.tts,
            SpeechServiceStatusSnapshot::Ready
        );

        // 7. Re-enable legacy ASR profile
        let reactivated = set_active_profile(
            &source,
            SetActiveSpeechProfileRequest {
                expected_revision: disabled_asr.configuration.revision.clone(),
                capability: SpeechCapabilityKind::Asr,
                profile_id: Some(LEGACY_ASR_PROFILE_ID.to_owned()),
            },
        )
        .await
        .expect("reactivate legacy asr");
        assert_eq!(
            reactivated.configuration.active_asr_id.as_deref(),
            Some(LEGACY_ASR_PROFILE_ID)
        );
        assert_eq!(
            reactivated.compiled.status.asr,
            SpeechServiceStatusSnapshot::Ready
        );

        // 8. Delete active TTS profile -> deactivates TTS while keeping legacy TTS profile intact
        let deleted_tts = delete_profile(
            &source,
            DeleteSpeechProfileRequest {
                expected_revision: reactivated.configuration.revision.clone(),
                capability: SpeechCapabilityKind::Tts,
                profile_id: ds_tts_id,
            },
        )
        .await
        .expect("delete active tts");
        assert_eq!(deleted_tts.configuration.tts_profiles.len(), 1);
        assert_eq!(deleted_tts.configuration.active_tts_id, None);
        assert_eq!(
            deleted_tts.compiled.status.tts,
            SpeechServiceStatusSnapshot::Unavailable
        );
        assert_eq!(deleted_tts.configuration.tts_profiles[0].id, LEGACY_TTS_PROFILE_ID);
    }
}
