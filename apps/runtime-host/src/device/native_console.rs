//! 直连终端原生控制面板、持久化 ACL/声纹存储、192 维 FBank 声纹提取与本机回环 MCP 桥接。
//!
//! 当终端（如 ESP32-S31 Mosaico）无需 Go 网关直接连接 EZ Assistant 时，
//! Host 复用已认证的 WSS `/device` 通道完成硬件控制与加密 NVS 声纹同步，
//! 并为桌面端详情页提供与原网关控制台一致的 `/api/v1/*` 接口以及受控的本机 `127.0.0.1` MCP 服务。

use std::{
    collections::BTreeMap,
    f32::consts::PI,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use assistant_protocol::{DeviceConsoleModeSnapshot, DeviceConsoleSnapshot, DeviceId};
use axum::{
    Json,
    body::Bytes,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, Method, StatusCode, header::AUTHORIZATION},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::{
    gateway::GatewayShared,
    multinet_pinyin,
    protocol::{
        DeviceCommandRequest, DeviceHardwareDeclaration, DeviceWakeConfig, VoiceprintSyncRequest,
        VoiceprintSyncTemplate, WakeConfigSyncRequest, WakeMode,
    },
};
use crate::{
    platform,
    speech::{SpeechServiceError, SpeechServiceHandle, wav_bytes_to_pcm16_16k_mono},
};

const DEVICE_DIR: &str = "device";
const STORE_FILE: &str = "native_console.json";
const DEFAULT_ACL_RULE_ID: &str = "default-ez-assistant-owner";
const EMBEDDING_DIM: usize = 192;
const FBANK_SAMPLE_RATE: usize = 16_000;
const FBANK_WIN_SAMPLES: usize = 400; // 25ms @ 16kHz
const FBANK_HOP_SAMPLES: usize = 160; // 10ms @ 16kHz
const FBANK_FFT_SIZE: usize = 512;
const FBANK_NUM_MEL_BINS: usize = 40;
const FBANK_F_MIN_HZ: f32 = 60.0;
const FBANK_F_MAX_HZ: f32 = 7600.0;
const FBANK_PRE_EMPHASIS: f32 = 0.97;
const MAX_AUDIO_DECODED_BYTES: usize = 4 * 1024 * 1024;

/// 直连终端的持久化 Agent → Device ACL 规则。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct NativeAclRule {
    pub(super) rule_id: String,
    pub(super) agent_id: String,
    pub(super) group_id: String,
    pub(super) zone_pattern: String,
    pub(super) device_id: String,
    pub(super) capability_pattern: String,
    pub(super) min_speaker_role: i32,
    pub(super) allow: bool,
}

/// 直连终端的持久化说话人档案与 192 维声纹模板（支持多模板平均质心 + 子模板匹配）。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(super) struct NativeSpeakerRecord {
    pub(super) speaker_id: String,
    pub(super) display_name: String,
    pub(super) uid: String,
    pub(super) agent_id: String,
    pub(super) group_id: String,
    pub(super) role: i32,
    pub(super) tts_voice: String,
    pub(super) prompt: String,
    #[serde(default)]
    pub(super) embedding: Vec<f32>,
    #[serde(default)]
    pub(super) templates: Vec<Vec<f32>>,
    pub(super) version: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct NativeConsoleDocument {
    schema_version: u32,
    vp_version: u64,
    #[serde(default)]
    wake: DeviceWakeConfig,
    #[serde(default)]
    acls: BTreeMap<String, NativeAclRule>,
    #[serde(default)]
    speakers: BTreeMap<String, NativeSpeakerRecord>,
}

impl Default for NativeConsoleDocument {
    fn default() -> Self {
        let mut acls = BTreeMap::new();
        acls.insert(
            DEFAULT_ACL_RULE_ID.to_owned(),
            NativeAclRule {
                rule_id: DEFAULT_ACL_RULE_ID.to_owned(),
                agent_id: "ez-assistant".to_owned(),
                group_id: "*".to_owned(),
                zone_pattern: "*".to_owned(),
                device_id: "*".to_owned(),
                capability_pattern: "*".to_owned(),
                min_speaker_role: 1,
                allow: true,
            },
        );
        Self {
            schema_version: 1,
            vp_version: 0,
            wake: DeviceWakeConfig::default(),
            acls,
            speakers: BTreeMap::new(),
        }
    }
}

/// Host 进程内持有的原生直连控制台持久化存储（ACL + 声纹库）。
pub(super) struct NativeConsoleStore {
    path: PathBuf,
    doc: Mutex<NativeConsoleDocument>,
}

impl NativeConsoleStore {
    pub(super) fn load_or_default(runtime_home: &Path) -> Self {
        let path = runtime_home.join(DEVICE_DIR).join(STORE_FILE);
        let loaded = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<NativeConsoleDocument>(&bytes).ok())
            .filter(|doc| doc.schema_version == 1)
            .unwrap_or_default();
        Self {
            path,
            doc: Mutex::new(loaded),
        }
    }

    pub(super) async fn vp_version(&self) -> u64 {
        self.doc.lock().await.vp_version
    }

    pub(super) async fn list_acls(&self) -> Vec<NativeAclRule> {
        self.doc.lock().await.acls.values().cloned().collect()
    }

    pub(super) async fn upsert_acl(&self, rule: NativeAclRule) -> Result<NativeAclRule, String> {
        let mut doc = self.doc.lock().await;
        doc.acls.insert(rule.rule_id.clone(), rule.clone());
        save_document(&self.path, &doc)?;
        Ok(rule)
    }

    pub(super) async fn delete_acl(&self, rule_id: &str) -> Result<bool, String> {
        let mut doc = self.doc.lock().await;
        let removed = doc.acls.remove(rule_id).is_some();
        if removed {
            save_document(&self.path, &doc)?;
        }
        Ok(removed)
    }

    pub(super) async fn check_access(
        &self,
        agent_id: &str,
        speaker_role: i32,
        hardware: &DeviceHardwareDeclaration,
        device_id: &str,
        capability: &str,
    ) -> Result<(), String> {
        let doc = self.doc.lock().await;
        let mut allowed = false;
        let canonical_cap = canonical_capability(capability);
        for rule in doc.acls.values() {
            if !rule_matches(rule, agent_id, hardware, device_id, capability, canonical_cap) {
                continue;
            }
            if !rule.allow {
                return Err(format!(
                    "ACL 策略拒绝：规则 `{}` 禁止 `{agent_id}` 调用 `{capability}`。",
                    rule.rule_id
                ));
            }
            if role_rank(speaker_role) >= role_rank(rule.min_speaker_role) {
                allowed = true;
            }
        }
        if !allowed {
            return Err(format!(
                "ACL 策略拒绝：`{agent_id}`（角色 {speaker_role}）未被授权调用 `{capability}`。"
            ));
        }
        Ok(())
    }

    pub(super) async fn list_speakers(&self) -> Vec<NativeSpeakerRecord> {
        self.doc.lock().await.speakers.values().cloned().collect()
    }

    pub(super) async fn wake_config(&self) -> (DeviceWakeConfig, u32) {
        let doc = self.doc.lock().await;
        let enrolled = enrolled_count_locked(&doc);
        let mut cfg = doc.wake.clone();
        if enrolled == 0 && cfg.mode != WakeMode::WakeWord {
            cfg.mode = WakeMode::WakeWord;
        }
        (cfg, enrolled)
    }

    pub(super) async fn build_wake_sync(&self) -> WakeConfigSyncRequest {
        let (config, enrolled_speaker_count) = self.wake_config().await;
        WakeConfigSyncRequest {
            sync_id: format!("wake-{}", random_hex_short()),
            config,
            enrolled_speaker_count,
        }
    }

    pub(super) async fn update_wake_config(
        &self,
        mut next: DeviceWakeConfig,
    ) -> Result<(DeviceWakeConfig, DeviceWakeConfig, u32), String> {
        let mut doc = self.doc.lock().await;
        let enrolled = enrolled_count_locked(&doc);
        if next.mode != WakeMode::WakeWord && enrolled == 0 {
            return Err("请先录入至少 1 位说话人声纹，再开启声纹唤醒模式。".to_owned());
        }
        next.wake_word = {
            let trimmed = next.wake_word.trim();
            if trimmed.is_empty() {
                multinet_pinyin::DEFAULT_WAKE_WORD.to_owned()
            } else if trimmed.chars().count() > 24 {
                return Err("唤醒词展示文本不能超过 24 个字符。".to_owned());
            } else {
                trimmed.to_owned()
            }
        };
        next.wake_pinyin = multinet_pinyin::normalize_and_validate_pinyin(&next.wake_pinyin)?;
        next.vp_threshold = next.vp_threshold.clamp(0.55, 0.95);
        next.min_speech_ms = next.min_speech_ms.clamp(400, 1500);
        next.cooldown_ms = next.cooldown_ms.clamp(500, 5000);

        let previous = doc.wake.clone();
        doc.wake = next.clone();
        save_document(&self.path, &doc)?;
        Ok((next, previous, enrolled))
    }

    pub(super) async fn restore_wake_config(&self, previous: DeviceWakeConfig) {
        let mut doc = self.doc.lock().await;
        doc.wake = previous;
        let _ = save_document(&self.path, &doc);
    }

    pub(super) async fn upsert_speaker_metadata(
        &self,
        mut record: NativeSpeakerRecord,
    ) -> Result<(NativeSpeakerRecord, VoiceprintSyncRequest), String> {
        let mut doc = self.doc.lock().await;
        if let Some(existing) = doc.speakers.get(&record.speaker_id) {
            if record.embedding.is_empty() {
                record.embedding = existing.embedding.clone();
            }
            if record.templates.is_empty() {
                record.templates = existing.templates.clone();
            }
        }
        doc.vp_version = doc.vp_version.saturating_add(1);
        record.version = doc.vp_version;
        doc.speakers
            .insert(record.speaker_id.clone(), record.clone());
        save_document(&self.path, &doc)?;
        let sync = build_sync_request(&doc);
        Ok((record, sync))
    }

    #[cfg(test)]
    pub(super) async fn enroll_speaker(
        &self,
        record: NativeSpeakerRecord,
        embedding: Vec<f32>,
    ) -> Result<(NativeSpeakerRecord, VoiceprintSyncRequest), String> {
        self.enroll_speaker_multi(record, vec![embedding]).await
    }

    pub(super) async fn enroll_speaker_multi(
        &self,
        mut record: NativeSpeakerRecord,
        raw_embeddings: Vec<Vec<f32>>,
    ) -> Result<(NativeSpeakerRecord, VoiceprintSyncRequest), String> {
        let mut normalized: Vec<Vec<f32>> = Vec::new();
        for mut emb in raw_embeddings.into_iter().take(3) {
            if emb.len() != EMBEDDING_DIM || emb.iter().any(|v| !v.is_finite()) {
                continue;
            }
            l2_normalize(&mut emb);
            normalized.push(emb);
        }
        if normalized.is_empty() {
            return Err("未提取到有效的 192 维声纹特征。".to_owned());
        }
        let mut centroid = vec![0.0_f32; EMBEDDING_DIM];
        for emb in &normalized {
            for (slot, &v) in centroid.iter_mut().zip(emb.iter()) {
                *slot += v;
            }
        }
        l2_normalize(&mut centroid);

        let mut doc = self.doc.lock().await;
        if record.prompt.is_empty()
            && let Some(existing) = doc.speakers.get(&record.speaker_id)
            && !existing.prompt.is_empty()
        {
            record.prompt = existing.prompt.clone();
        }
        doc.vp_version = doc.vp_version.saturating_add(1);
        record.version = doc.vp_version;
        record.embedding = centroid;
        record.templates = normalized;
        doc.speakers
            .insert(record.speaker_id.clone(), record.clone());
        save_document(&self.path, &doc)?;
        let sync = build_sync_request(&doc);
        Ok((record, sync))
    }

    pub(super) async fn delete_speaker(
        &self,
        speaker_id: &str,
    ) -> Result<(bool, Option<VoiceprintSyncRequest>, Option<WakeConfigSyncRequest>), String> {
        let mut doc = self.doc.lock().await;
        let removed = doc.speakers.remove(speaker_id).is_some();
        if !removed {
            return Ok((false, None, None));
        }
        doc.vp_version = doc.vp_version.saturating_add(1);
        let remaining_enrolled = enrolled_count_locked(&doc);
        let fallback_wake = if remaining_enrolled == 0 && doc.wake.mode != WakeMode::WakeWord {
            doc.wake.mode = WakeMode::WakeWord;
            Some(WakeConfigSyncRequest {
                sync_id: format!("wake-{}", random_hex_short()),
                config: doc.wake.clone(),
                enrolled_speaker_count: 0,
            })
        } else {
            None
        };
        save_document(&self.path, &doc)?;
        Ok((true, Some(build_sync_request(&doc)), fallback_wake))
    }

    /// 设备完成 `hello` 上线后对齐端侧声纹版本：
    /// - 若终端上报了更高的旧版 `vp_version`（例如曾连过其他网关/主机），先抬高本机版本下限，
    ///   保证后续首次声纹注册（`device_vp_version + 1`）或本次全量下发一定能被终端 `ApplySync` 接受；
    /// - 若双方版本已一致则不下发，避免终端每次重连重复擦写加密 NVS Flash。
    pub(super) async fn prepare_initial_sync(
        &self,
        device_vp_version: u64,
    ) -> Option<VoiceprintSyncRequest> {
        let mut doc = self.doc.lock().await;
        if doc.vp_version < device_vp_version {
            doc.vp_version = device_vp_version;
            let _ = save_document(&self.path, &doc);
            if doc.speakers.is_empty() {
                return None;
            }
            return Some(build_sync_request(&doc));
        }
        if doc.vp_version == device_vp_version {
            return None;
        }
        Some(build_sync_request(&doc))
    }

    pub(super) async fn identify_embedding(
        &self,
        query: &[f32],
    ) -> (bool, Option<NativeSpeakerRecord>, f32) {
        let doc = self.doc.lock().await;
        let mut best_score = 0.0_f32;
        let mut best: Option<NativeSpeakerRecord> = None;
        for speaker in doc.speakers.values() {
            let mut speaker_best = 0.0_f32;
            if speaker.embedding.len() == query.len() {
                speaker_best = speaker_best.max(cosine_similarity(query, &speaker.embedding));
            }
            for tpl in &speaker.templates {
                if tpl.len() == query.len() {
                    speaker_best = speaker_best.max(cosine_similarity(query, tpl));
                }
            }
            if speaker_best > best_score {
                best_score = speaker_best;
                best = Some(speaker.clone());
            }
        }
        let matched = best.is_some() && best_score >= 0.65;
        (matched, best, best_score)
    }
}

fn enrolled_count_locked(doc: &NativeConsoleDocument) -> u32 {
    doc.speakers
        .values()
        .filter(|s| s.embedding.len() == EMBEDDING_DIM)
        .count() as u32
}

pub(super) fn native_console_snapshot() -> DeviceConsoleSnapshot {
    DeviceConsoleSnapshot {
        api_prefix: "/api/v1".to_owned(),
        mode: DeviceConsoleModeSnapshot::Native,
        ui_path: None,
        direct_url: None,
    }
}

fn build_sync_request(doc: &NativeConsoleDocument) -> VoiceprintSyncRequest {
    let mut upserts = Vec::new();
    for speaker in doc.speakers.values() {
        if speaker.embedding.len() != EMBEDDING_DIM {
            continue;
        }
        upserts.push(VoiceprintSyncTemplate {
            speaker_id: speaker.speaker_id.clone(),
            display_name: speaker.display_name.clone(),
            role: speaker.role,
            embedding: speaker.embedding.clone(),
            version: speaker.version,
        });
        if speaker.templates.len() >= 2 {
            // 同步最多 2 条额外子模板（`speaker_id#2`, `speaker_id#3`），兼顾跨句子召回与 NVS 体积。
            let mut extra_indices = vec![0usize];
            let last = speaker.templates.len() - 1;
            if last != 0 {
                extra_indices.push(last);
            }
            for (slot_idx, &tpl_idx) in extra_indices.iter().take(2).enumerate() {
                let tpl = &speaker.templates[tpl_idx];
                if tpl.len() == EMBEDDING_DIM {
                    upserts.push(VoiceprintSyncTemplate {
                        speaker_id: format!("{}#{}", speaker.speaker_id, slot_idx + 2),
                        display_name: speaker.display_name.clone(),
                        role: speaker.role,
                        embedding: tpl.clone(),
                        version: speaker.version,
                    });
                }
            }
        }
    }
    VoiceprintSyncRequest {
        group_id: "home-default".to_owned(),
        vp_version: doc.vp_version,
        full_sync: true,
        upserts,
        deleted_speaker_ids: Vec::new(),
    }
}

fn save_document(path: &Path, doc: &NativeConsoleDocument) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("创建设备目录失败：{err}"))?;
    }
    let bytes = serde_json::to_vec_pretty(doc).map_err(|err| format!("序列化失败：{err}"))?;
    let mut suffix_bytes = [0_u8; 6];
    let _ = getrandom::fill(&mut suffix_bytes);
    let suffix = hex_bytes(&suffix_bytes);
    let tmp_path = path.with_file_name(format!(".{STORE_FILE}.{suffix}.tmp"));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    platform::apply_private_open_options(&mut opts);
    let mut file = opts
        .open(&tmp_path)
        .map_err(|err| format!("写入临时文件失败：{err}"))?;
    file.write_all(&bytes)
        .map_err(|err| format!("保存控制台配置失败：{err}"))?;
    let _ = file.sync_all();
    drop(file);
    fs::rename(&tmp_path, path).map_err(|err| format!("替换控制台配置失败：{err}"))?;
    Ok(())
}

fn hex_bytes(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

fn role_rank(role: i32) -> i32 {
    match role {
        1 => 3, // Owner
        2 => 2, // Family / Member
        3 => 1, // Guest
        _ => 0,
    }
}

fn match_glob(pattern: &str, value: &str) -> bool {
    let trimmed = pattern.trim();
    if trimmed.is_empty() || trimmed == "*" {
        return true;
    }
    if let Some(prefix) = trimmed.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    trimmed.eq_ignore_ascii_case(value)
}

fn canonical_capability(cap: &str) -> &str {
    match cap.trim() {
        "camera.capture" => "device.camera_capture",
        "audio.volume" | "audio.set_volume" | "audio.output" => "device.audio_set_volume",
        "gpio.set" | "actuator.set" | "actuator.led" => "device.actuator_set",
        "sensor.read" | "sensor.env" => "device.sensor_read",
        "lcd.display" | "display.lcd" | "display.status" => "device.display_show",
        "voiceprint.list" => "device.voiceprint_list",
        "voiceprint.delete" => "device.voiceprint_delete",
        "status.read" => "device.status_read",
        other => other,
    }
}

fn rule_matches(
    rule: &NativeAclRule,
    agent_id: &str,
    hardware: &DeviceHardwareDeclaration,
    device_id: &str,
    raw_cap: &str,
    canonical_cap: &str,
) -> bool {
    if !match_glob(&rule.agent_id, agent_id) {
        return false;
    }
    if !match_glob(&rule.group_id, &hardware.group_id) {
        return false;
    }
    if !match_glob(&rule.zone_pattern, &hardware.zone) {
        return false;
    }
    let dev_match = rule.device_id.trim().is_empty()
        || rule.device_id.trim() == "*"
        || rule.device_id.eq_ignore_ascii_case(&hardware.mac)
        || rule.device_id.eq_ignore_ascii_case(device_id);
    if !dev_match {
        return false;
    }
    match_glob(&rule.capability_pattern, raw_cap)
        || match_glob(&rule.capability_pattern, canonical_cap)
}

/// 处理桌面端 `/devices/{device_id}/console/{*path}` 针对直连终端的原生 API 请求。
pub(crate) async fn handle_native_console_request(
    shared: &Arc<GatewayShared>,
    device_id: &DeviceId,
    relative_path: &str,
    method: Method,
    body: Bytes,
    speech: &SpeechServiceHandle,
) -> Response {
    let trimmed = relative_path.trim_start_matches('/');
    let subpath = trimmed
        .strip_prefix("api/v1/")
        .or_else(|| trimmed.strip_prefix("api/v1"))
        .unwrap_or(trimmed)
        .trim_start_matches('/');

    let Some(hardware) = shared.native_hardware(device_id).await else {
        return json_error(StatusCode::NOT_FOUND, "直连终端当前离线或不支持原生控制台。");
    };

    if subpath == "devices" && method == Method::GET {
        return handle_list_devices(shared, device_id, &hardware).await;
    }

    if let Some(rest) = subpath.strip_prefix("devices/")
        && let Some(_target) = rest.strip_suffix("/commands")
        && method == Method::POST
    {
        return handle_device_command(shared, device_id, &hardware, body).await;
    }

    if subpath == "acls" && method == Method::GET {
        let rules = shared.native_console.list_acls().await;
        return (StatusCode::OK, Json(json!({ "rules": rules }))).into_response();
    }

    if let Some(raw_id) = subpath.strip_prefix("acls/") {
        let rule_id = percent_decode_segment(raw_id);
        if rule_id.is_empty() {
            return json_error(StatusCode::BAD_REQUEST, "规则 ID 不能为空。");
        }
        return match method {
            Method::PUT | Method::POST => handle_upsert_acl(shared, rule_id, body).await,
            Method::DELETE => match shared.native_console.delete_acl(&rule_id).await {
                Ok(deleted) => (StatusCode::OK, Json(json!({ "deleted": deleted }))).into_response(),
                Err(msg) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &msg),
            },
            _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
        };
    }

    if subpath == "wake" {
        return match method {
            Method::GET => handle_get_wake(shared, &hardware).await,
            Method::PUT | Method::POST => handle_update_wake(shared, device_id, body).await,
            _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
        };
    }

    if subpath == "speakers" && method == Method::GET {
        let speakers = shared
            .native_console
            .list_speakers()
            .await
            .into_iter()
            .map(speaker_to_json)
            .collect::<Vec<_>>();
        return (StatusCode::OK, Json(json!({ "speakers": speakers }))).into_response();
    }

    if let Some(raw_id) = subpath.strip_prefix("speakers/") {
        let speaker_id = percent_decode_segment(raw_id);
        if speaker_id.is_empty() {
            return json_error(StatusCode::BAD_REQUEST, "说话人 ID 不能为空。");
        }
        return match method {
            Method::PUT | Method::POST => handle_upsert_speaker(shared, speaker_id, body).await,
            Method::DELETE => match shared.native_console.delete_speaker(&speaker_id).await {
                Ok((deleted, maybe_sync, maybe_wake_fallback)) => {
                    if let Some(sync) = maybe_sync {
                        shared.broadcast_voiceprint_sync(sync).await;
                    }
                    let auto_fallback_wake_word = maybe_wake_fallback.is_some();
                    if let Some(wake_sync) = maybe_wake_fallback {
                        shared.broadcast_wake_config_sync(wake_sync).await;
                    }
                    (
                        StatusCode::OK,
                        Json(json!({
                            "deleted": deleted,
                            "auto_fallback_wake_word": auto_fallback_wake_word,
                        })),
                    )
                        .into_response()
                }
                Err(msg) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &msg),
            },
            _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
        };
    }

    if subpath == "voice/capture_embedding" && method == Method::POST {
        return handle_voice_capture_embedding(shared, device_id, body).await;
    }

    if subpath == "voice/enroll" && method == Method::POST {
        return handle_voice_enroll(shared, body).await;
    }

    if subpath == "voice/identify" && method == Method::POST {
        return handle_voice_identify(shared, body).await;
    }

    if subpath == "voice/transcribe" && method == Method::POST {
        return handle_voice_transcribe(speech, body).await;
    }

    json_error(StatusCode::NOT_FOUND, "不支持的直连终端控制台接口。")
}

async fn handle_list_devices(
    shared: &Arc<GatewayShared>,
    device_id: &DeviceId,
    hardware: &DeviceHardwareDeclaration,
) -> Response {
    let mut rssi_val: Value = Value::Null;
    let mut volume_val: Value = Value::Null;
    let mut battery_pct: Value = Value::Null;
    let mut temperature_c: Value = Value::Null;
    let mut free_int: Value = Value::Null;
    let mut vp_version = hardware.vp_version;

    // 尝试通过 WSS 拉取实时硬件遥测（2 秒超时，失败时无缝回退静态档案）
    let status_req = DeviceCommandRequest {
        command_id: format!("status-{}", random_hex_short()),
        capability: "device.status_read".to_owned(),
        action: "read".to_owned(),
        params_json: "{}".to_owned(),
        timeout_ms: 2000,
    };
    if let Ok(res) = shared
        .execute_device_command(device_id, status_req, Duration::from_secs(2))
        .await
        && res.ok
        && let Ok(parsed) = serde_json::from_str::<Value>(&res.result_json)
    {
        if let Some(v) = parsed.get("rssi") {
            rssi_val = v.clone();
        }
        if let Some(v) = parsed.get("volume") {
            volume_val = v.clone();
        }
        if let Some(v) = parsed.get("battery_pct") {
            battery_pct = v.clone();
        }
        if let Some(v) = parsed.get("temperature_c") {
            temperature_c = v.clone();
        }
        if let Some(v) = parsed.get("free_int") {
            free_int = v.clone();
        }
        if let Some(v) = parsed.get("vp_version").and_then(Value::as_u64) {
            vp_version = v;
        }
    }

    let store_vp = shared.native_console.vp_version().await;
    let effective_vp = vp_version.max(store_vp);
    let target_id = if hardware.mac.is_empty() {
        device_id.as_str()
    } else {
        hardware.mac.as_str()
    };
    let capabilities = hardware
        .capabilities
        .iter()
        .map(|name| json!({ "name": name }))
        .collect::<Vec<_>>();

    (
        StatusCode::OK,
        Json(json!({
            "devices": [
                {
                    "hello": {
                        "device_id": target_id,
                        "mac": hardware.mac,
                        "group_id": if hardware.group_id.is_empty() { "home-default" } else { &hardware.group_id },
                        "zone": if hardware.zone.is_empty() { "desk" } else { &hardware.zone },
                        "chip": if hardware.chip.is_empty() { "esp32s31" } else { &hardware.chip },
                        "model": hardware.model,
                        "vp_version": effective_vp,
                        "capabilities": capabilities,
                    },
                    "shadow": {
                        "device_id": target_id,
                        "zone": if hardware.zone.is_empty() { "desk" } else { &hardware.zone },
                        "group_id": if hardware.group_id.is_empty() { "home-default" } else { &hardware.group_id },
                        "online": true,
                        "vad_state": "idle",
                        "rssi": rssi_val,
                        "volume": volume_val,
                        "battery_pct": battery_pct,
                        "temperature_c": temperature_c,
                        "free_int": free_int,
                    }
                }
            ]
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct ConsoleCommandPayload {
    #[serde(default)]
    agent_id: String,
    capability: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    params_json: String,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u32,
}

fn default_timeout_ms() -> u32 {
    5000
}

async fn handle_device_command(
    shared: &Arc<GatewayShared>,
    device_id: &DeviceId,
    hardware: &DeviceHardwareDeclaration,
    body: Bytes,
) -> Response {
    let payload: ConsoleCommandPayload = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "指令 JSON 格式无效。"),
    };
    let capability = payload.capability.trim();
    if capability.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "capability 不能为空。");
    }
    let agent_id = if payload.agent_id.trim().is_empty() {
        "ez-assistant"
    } else {
        payload.agent_id.trim()
    };
    if let Err(reason) = shared
        .native_console
        .check_access(agent_id, 1, hardware, device_id.as_str(), capability)
        .await
    {
        return json_error(StatusCode::FORBIDDEN, &reason);
    }

    let timeout_ms = payload.timeout_ms.clamp(500, 15_000);
    let req = DeviceCommandRequest {
        command_id: format!("cmd-{}", random_hex_short()),
        capability: canonical_capability(capability).to_owned(),
        action: if payload.action.trim().is_empty() {
            default_action_for_capability(capability).to_owned()
        } else {
            payload.action.trim().to_owned()
        },
        params_json: if payload.params_json.trim().is_empty() {
            "{}".to_owned()
        } else {
            payload.params_json
        },
        timeout_ms,
    };

    match shared
        .execute_device_command(device_id, req, Duration::from_millis(u64::from(timeout_ms)))
        .await
    {
        Ok(res) => (
            StatusCode::OK,
            Json(json!({
                "command_id": res.command_id,
                "ok": res.ok,
                "result_json": res.result_json,
                "media_url": res.media_url,
                "error_code": res.error_code,
                "error_message": res.error_message,
            })),
        )
            .into_response(),
        Err(msg) => json_error(StatusCode::BAD_GATEWAY, &msg),
    }
}

#[derive(Deserialize)]
struct UpsertAclPayload {
    #[serde(default)]
    agent_id: String,
    #[serde(default)]
    group_id: String,
    #[serde(default)]
    zone_pattern: String,
    #[serde(default)]
    device_id: String,
    #[serde(default)]
    capability_pattern: String,
    #[serde(default = "default_owner_role")]
    min_speaker_role: i32,
    #[serde(default = "default_true")]
    allow: bool,
}

fn default_owner_role() -> i32 {
    1
}

fn default_true() -> bool {
    true
}

async fn handle_upsert_acl(
    shared: &Arc<GatewayShared>,
    rule_id: String,
    body: Bytes,
) -> Response {
    let payload: UpsertAclPayload = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "ACL 规则 JSON 格式无效。"),
    };
    let rule = NativeAclRule {
        rule_id,
        agent_id: non_empty_or(payload.agent_id, "*"),
        group_id: non_empty_or(payload.group_id, "*"),
        zone_pattern: non_empty_or(payload.zone_pattern, "*"),
        device_id: non_empty_or(payload.device_id, "*"),
        capability_pattern: non_empty_or(payload.capability_pattern, "*"),
        min_speaker_role: payload.min_speaker_role.clamp(1, 3),
        allow: payload.allow,
    };
    match shared.native_console.upsert_acl(rule).await {
        Ok(saved) => (StatusCode::OK, Json(json!({ "rule": saved }))).into_response(),
        Err(msg) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &msg),
    }
}

#[derive(Deserialize)]
struct UpsertSpeakerPayload {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    uid: String,
    #[serde(default)]
    agent_id: String,
    #[serde(default)]
    group_id: String,
    #[serde(default = "default_owner_role")]
    role: i32,
    #[serde(default)]
    tts_voice: String,
    #[serde(default)]
    prompt: String,
}

async fn handle_upsert_speaker(
    shared: &Arc<GatewayShared>,
    speaker_id: String,
    body: Bytes,
) -> Response {
    let payload: UpsertSpeakerPayload = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "说话人配置 JSON 格式无效。"),
    };
    let display_name = if payload.display_name.trim().is_empty() {
        speaker_id.clone()
    } else {
        payload.display_name.trim().to_owned()
    };
    let record = NativeSpeakerRecord {
        speaker_id,
        display_name,
        uid: non_empty_or(payload.uid, "uid-owner-1"),
        agent_id: non_empty_or(payload.agent_id, "ez-assistant"),
        group_id: non_empty_or(payload.group_id, "home-default"),
        role: payload.role.clamp(1, 3),
        tts_voice: payload.tts_voice.trim().to_owned(),
        prompt: payload.prompt.trim().to_owned(),
        embedding: Vec::new(),
        templates: Vec::new(),
        version: 0,
    };
    match shared.native_console.upsert_speaker_metadata(record).await {
        Ok((saved, sync)) => {
            shared.broadcast_voiceprint_sync(sync).await;
            (StatusCode::OK, Json(json!({ "speaker": speaker_to_json(saved) }))).into_response()
        }
        Err(msg) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &msg),
    }
}

async fn handle_get_wake(
    shared: &Arc<GatewayShared>,
    hardware: &DeviceHardwareDeclaration,
) -> Response {
    let (wake, enrolled_speaker_count) = shared.native_console.wake_config().await;
    (
        StatusCode::OK,
        Json(json!({
            "wake": wake,
            "enrolled_speaker_count": enrolled_speaker_count,
            "multinet_ready": hardware.multinet_ready,
            "device_wake_config": hardware.wake_config,
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct UpdateWakePayload {
    #[serde(default)]
    mode: Option<WakeMode>,
    #[serde(default)]
    wake_word: Option<String>,
    #[serde(default)]
    wake_pinyin: Option<String>,
    #[serde(default)]
    vp_threshold: Option<f32>,
    #[serde(default)]
    min_speech_ms: Option<u32>,
    #[serde(default)]
    cooldown_ms: Option<u32>,
    #[serde(default)]
    vp_wake_chime: Option<bool>,
}

async fn handle_update_wake(
    shared: &Arc<GatewayShared>,
    device_id: &DeviceId,
    body: Bytes,
) -> Response {
    let payload: UpdateWakePayload = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "唤醒配置 JSON 格式无效。"),
    };
    let (mut next, _) = shared.native_console.wake_config().await;
    if let Some(mode) = payload.mode {
        next.mode = mode;
    }
    if let Some(word) = payload.wake_word {
        next.wake_word = word;
    }
    if let Some(pinyin) = payload.wake_pinyin {
        next.wake_pinyin = pinyin;
    }
    if let Some(th) = payload.vp_threshold {
        next.vp_threshold = th;
    }
    if let Some(ms) = payload.min_speech_ms {
        next.min_speech_ms = ms;
    }
    if let Some(ms) = payload.cooldown_ms {
        next.cooldown_ms = ms;
    }
    if let Some(chime) = payload.vp_wake_chime {
        next.vp_wake_chime = chime;
    }

    let (saved, previous, enrolled) = match shared.native_console.update_wake_config(next).await {
        Ok(v) => v,
        Err(msg) => return json_error(StatusCode::BAD_REQUEST, &msg),
    };

    let sync_req = WakeConfigSyncRequest {
        sync_id: format!("wake-{}", random_hex_short()),
        config: saved.clone(),
        enrolled_speaker_count: enrolled,
    };
    match shared
        .sync_wake_config(device_id, sync_req, Duration::from_secs(4))
        .await
    {
        Ok(ack) if ack.ok => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "synced": true,
                "wake": saved,
                "enrolled_speaker_count": enrolled,
                "device_wake_config": ack.active_config,
            })),
        )
            .into_response(),
        Ok(ack) => {
            shared.native_console.restore_wake_config(previous).await;
            let err = ack
                .error
                .unwrap_or_else(|| "终端 MultiNet7 拒绝应用该唤醒词拼音。".to_owned());
            json_error(StatusCode::BAD_REQUEST, &err)
        }
        Err(msg) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "synced": false,
                "sync_warning": msg,
                "wake": saved,
                "enrolled_speaker_count": enrolled,
            })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct CaptureEmbeddingPayload {
    #[serde(default = "default_capture_duration_ms")]
    duration_ms: u32,
    #[serde(default)]
    step: u32,
    #[serde(default)]
    total: u32,
}

fn default_capture_duration_ms() -> u32 {
    3000
}

async fn handle_voice_capture_embedding(
    shared: &Arc<GatewayShared>,
    device_id: &DeviceId,
    body: Bytes,
) -> Response {
    let payload: CaptureEmbeddingPayload = if body.is_empty() {
        CaptureEmbeddingPayload {
            duration_ms: default_capture_duration_ms(),
            step: 1,
            total: 3,
        }
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "声纹采集请求格式无效。"),
        }
    };
    let duration_ms = payload.duration_ms.clamp(1200, 4500);
    let params_json = json!({
        "duration_ms": duration_ms,
        "step": payload.step.clamp(1, 5),
        "total": payload.total.clamp(1, 5),
    })
    .to_string();
    let req = DeviceCommandRequest {
        command_id: format!("vpcap-{}", random_hex_short()),
        capability: "device.voiceprint_capture".to_owned(),
        action: "capture".to_owned(),
        params_json,
        timeout_ms: duration_ms.saturating_add(4500),
    };
    match shared
        .execute_device_command(
            device_id,
            req,
            Duration::from_millis(u64::from(duration_ms.saturating_add(5000))),
        )
        .await
    {
        Ok(res) if res.ok => {
            let parsed: Value = serde_json::from_str(&res.result_json)
                .unwrap_or_else(|_| json!({ "raw": res.result_json }));
            let emb_len = parsed
                .get("embedding")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            if emb_len != EMBEDDING_DIM {
                return json_error(
                    StatusCode::BAD_GATEWAY,
                    "终端返回的声纹特征维度异常，请重新录制。",
                );
            }
            (StatusCode::OK, Json(parsed)).into_response()
        }
        Ok(res) => {
            let msg = if res.error_message.is_empty() {
                "终端声纹录制失败，请靠近 Mosaico 重新说一句完整的话。".to_owned()
            } else {
                res.error_message
            };
            json_error(StatusCode::BAD_REQUEST, &msg)
        }
        Err(msg) => json_error(StatusCode::BAD_GATEWAY, &msg),
    }
}

#[derive(Deserialize)]
struct VoiceEnrollPayload {
    speaker_id: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    uid: String,
    #[serde(default)]
    agent_id: String,
    #[serde(default)]
    group_id: String,
    #[serde(default = "default_owner_role")]
    role: i32,
    #[serde(default)]
    tts_voice: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    audio_base64: String,
    #[serde(default)]
    audio_samples_base64: Vec<String>,
    #[serde(default)]
    embedding: Vec<f32>,
    #[serde(default)]
    embeddings: Vec<Vec<f32>>,
}

async fn handle_voice_enroll(shared: &Arc<GatewayShared>, body: Bytes) -> Response {
    let payload: VoiceEnrollPayload = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "声纹注册请求格式无效。"),
    };
    let speaker_id = payload.speaker_id.trim().to_owned();
    if speaker_id.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "speaker_id 不能为空。");
    }

    let mut extracted_embeddings: Vec<Vec<f32>> = Vec::new();
    if !payload.embeddings.is_empty() {
        for emb in payload.embeddings.into_iter().take(3) {
            if emb.len() == EMBEDDING_DIM && emb.iter().all(|v| v.is_finite()) {
                extracted_embeddings.push(emb);
            }
        }
    } else if payload.embedding.len() == EMBEDDING_DIM
        && payload.embedding.iter().all(|v| v.is_finite())
    {
        extracted_embeddings.push(payload.embedding);
    } else if !payload.audio_samples_base64.is_empty() {
        for sample_b64 in payload.audio_samples_base64.iter().take(3) {
            let (_, samples) = match decode_audio_base64_to_pcm16(sample_b64) {
                Ok(v) => v,
                Err(msg) => return json_error(StatusCode::BAD_REQUEST, &msg),
            };
            let Some(emb) = extract_fbank_embedding_192(&samples) else {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "存在过短的音频样本，请确保每条录音包含至少 0.5 秒以上有效语音。",
                );
            };
            extracted_embeddings.push(emb);
        }
    } else if !payload.audio_base64.trim().is_empty() {
        let (_, samples) = match decode_audio_base64_to_pcm16(&payload.audio_base64) {
            Ok(v) => v,
            Err(msg) => return json_error(StatusCode::BAD_REQUEST, &msg),
        };
        let Some(emb) = extract_fbank_embedding_192(&samples) else {
            return json_error(
                StatusCode::BAD_REQUEST,
                "音频样本过短，请录制至少 0.5 秒以上有效语音。",
            );
        };
        extracted_embeddings.push(emb);
    }

    if extracted_embeddings.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "请提供有效音频样本或 Mosaico 采集的 192 维声纹特征。",
        );
    }

    let display_name = if payload.display_name.trim().is_empty() {
        speaker_id.clone()
    } else {
        payload.display_name.trim().to_owned()
    };
    let record = NativeSpeakerRecord {
        speaker_id,
        display_name,
        uid: non_empty_or(payload.uid, "uid-owner-1"),
        agent_id: non_empty_or(payload.agent_id, "ez-assistant"),
        group_id: non_empty_or(payload.group_id, "home-default"),
        role: payload.role.clamp(1, 3),
        tts_voice: payload.tts_voice.trim().to_owned(),
        prompt: payload.prompt.trim().to_owned(),
        embedding: Vec::new(),
        templates: Vec::new(),
        version: 0,
    };
    let template_count = extracted_embeddings.len();
    match shared
        .native_console
        .enroll_speaker_multi(record, extracted_embeddings)
        .await
    {
        Ok((saved, sync)) => {
            let synced_devices = shared.broadcast_voiceprint_sync(sync).await;
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "speaker_id": saved.speaker_id,
                    "display_name": saved.display_name,
                    "role": saved.role,
                    "version": saved.version,
                    "embedding_dim": EMBEDDING_DIM,
                    "template_count": template_count,
                    "synced_devices": synced_devices,
                })),
            )
                .into_response()
        }
        Err(msg) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &msg),
    }
}

#[derive(Deserialize)]
struct VoiceIdentifyPayload {
    #[serde(default)]
    audio_base64: String,
    #[serde(default)]
    embedding: Vec<f32>,
}

async fn handle_voice_identify(shared: &Arc<GatewayShared>, body: Bytes) -> Response {
    let payload: VoiceIdentifyPayload = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "声纹识别请求格式无效。"),
    };
    let query = if payload.embedding.len() == EMBEDDING_DIM
        && payload.embedding.iter().all(|v| v.is_finite())
    {
        let mut q = payload.embedding;
        l2_normalize(&mut q);
        q
    } else {
        let (_, samples) = match decode_audio_base64_to_pcm16(&payload.audio_base64) {
            Ok(v) => v,
            Err(msg) => return json_error(StatusCode::BAD_REQUEST, &msg),
        };
        let Some(q) = extract_fbank_embedding_192(&samples) else {
            return json_error(StatusCode::BAD_REQUEST, "音频样本过短，无法提取声纹特征。");
        };
        q
    };
    let (matched, best, confidence) = shared.native_console.identify_embedding(&query).await;
    let tier = if confidence >= 0.85 {
        "local_high_confidence"
    } else if confidence >= 0.65 {
        "gray_zone"
    } else {
        "guest"
    };
    (
        StatusCode::OK,
        Json(json!({
            "matched": matched,
            "speaker_id": best.as_ref().map(|s| s.speaker_id.as_str()).unwrap_or(""),
            "display_name": best.as_ref().map(|s| s.display_name.as_str()).unwrap_or(""),
            "role": best.as_ref().map(|s| s.role).unwrap_or(3),
            "confidence": confidence,
            "tier": tier,
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct VoiceTranscribePayload {
    audio_base64: String,
}

async fn handle_voice_transcribe(speech: &SpeechServiceHandle, body: Bytes) -> Response {
    let payload: VoiceTranscribePayload = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "语音转写请求格式无效。"),
    };
    let (pcm_bytes, _) = match decode_audio_base64_to_pcm16(&payload.audio_base64) {
        Ok(v) => v,
        Err(msg) => return json_error(StatusCode::BAD_REQUEST, &msg),
    };
    match speech
        .recognize(
            pcm_bytes,
            "native-console-voice-test".to_owned(),
            CancellationToken::new(),
        )
        .await
    {
        Ok(text) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "text": text,
                "provider": "ez-assistant-host-asr",
            })),
        )
            .into_response(),
        Err(SpeechServiceError::Unavailable) => json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "尚未配置或启用语音识别（ASR），请先在「智能终端 → 语音服务」配置 ASR。",
        ),
        Err(SpeechServiceError::Busy) => {
            json_error(StatusCode::TOO_MANY_REQUESTS, "语音识别服务正忙，请稍后重试。")
        }
        Err(err) => json_error(StatusCode::BAD_GATEWAY, &format!("ASR 转写失败：{err:?}")),
    }
}

fn speaker_to_json(speaker: NativeSpeakerRecord) -> Value {
    let has_vp = speaker.embedding.len() == EMBEDDING_DIM;
    let template_count = speaker.templates.len().max(usize::from(has_vp));
    json!({
        "speaker_id": speaker.speaker_id,
        "display_name": speaker.display_name,
        "uid": speaker.uid,
        "agent_id": speaker.agent_id,
        "group_id": speaker.group_id,
        "role": speaker.role,
        "tts_voice": speaker.tts_voice,
        "prompt": speaker.prompt,
        "version": speaker.version,
        "has_voiceprint": has_vp,
        "template_count": template_count,
    })
}

/// 本机回环 Streamable HTTP MCP 桥接处理器：`POST /mcp/{device_id}`。
pub(super) async fn handle_mcp_http_request(
    State(shared): State<Arc<GatewayShared>>,
    AxumPath(raw_device_id): AxumPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let expected_token = match shared.active_mcp_token().await {
        Some(tok) => tok,
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let supplied = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    if !constant_time_eq(supplied.as_bytes(), expected_token.as_bytes()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let Ok(device_id) = DeviceId::new(raw_device_id) else {
        return rpc_error(Value::Null, -32602, "invalid device_id");
    };

    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return rpc_error(Value::Null, -32700, "parse error"),
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => {
            let negotiated = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .unwrap_or("2025-03-26");
            rpc_result(
                id,
                json!({
                    "protocolVersion": negotiated,
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "ez-direct-device-mcp",
                        "version": "1.0.0"
                    }
                }),
            )
        }
        "notifications/initialized" | "notifications/cancelled" => {
            StatusCode::ACCEPTED.into_response()
        }
        "ping" => rpc_result(id, json!({})),
        "tools/list" => {
            let Some(hardware) = shared.native_hardware(&device_id).await else {
                return rpc_result(id, json!({ "tools": [] }));
            };
            rpc_result(id, json!({ "tools": build_mcp_tools(&hardware) }))
        }
        "tools/call" => {
            let Some(hardware) = shared.native_hardware(&device_id).await else {
                return rpc_error(id, -32001, "target device is offline");
            };
            let tool_name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if tool_name.is_empty() {
                return rpc_error(id, -32602, "tool name is required");
            }
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));

            // AGENTS.md §4.4：Agent 调用任何硬件工具前强制校验 ACL（使用当前活跃说话人声纹角色）。
            let speaker_role = shared.active_speaker_role(&device_id).await;
            if let Err(reason) = shared
                .native_console
                .check_access(
                    "ez-assistant",
                    speaker_role,
                    &hardware,
                    device_id.as_str(),
                    tool_name,
                )
                .await
            {
                return rpc_result(
                    id,
                    json!({
                        "isError": true,
                        "content": [{ "type": "text", "text": reason }],
                    }),
                );
            }

            let action = args
                .get("action")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| default_action_for_capability(tool_name).to_owned());
            let params_json = serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_owned());
            let cmd = DeviceCommandRequest {
                command_id: format!("mcp-{}", random_hex_short()),
                capability: canonical_capability(tool_name).to_owned(),
                action,
                params_json,
                timeout_ms: 5000,
            };
            match shared
                .execute_device_command(&device_id, cmd, Duration::from_secs(5))
                .await
            {
                Ok(res) => {
                    let text = if res.ok {
                        if res.result_json.is_empty() {
                            "{\"ok\":true}".to_owned()
                        } else {
                            res.result_json
                        }
                    } else {
                        format!(
                            "{}: {}",
                            if res.error_code.is_empty() {
                                "DEVICE_ERROR"
                            } else {
                                &res.error_code
                            },
                            res.error_message
                        )
                    };
                    rpc_result(
                        id,
                        json!({
                            "isError": !res.ok,
                            "content": [{ "type": "text", "text": text }],
                        }),
                    )
                }
                Err(err) => rpc_result(
                    id,
                    json!({
                        "isError": true,
                        "content": [{ "type": "text", "text": err }],
                    }),
                ),
            }
        }
        _ => rpc_error(id, -32601, &format!("method not found: {method}")),
    }
}

fn build_mcp_tools(hardware: &DeviceHardwareDeclaration) -> Vec<Value> {
    let has = |cap: &str| {
        hardware.capabilities.is_empty()
            || hardware
                .capabilities
                .iter()
                .any(|c| canonical_capability(c) == cap)
    };
    let mut tools = Vec::new();
    if has("device.display_show") {
        tools.push(json!({
            "name": "device.display_show",
            "description": "更新 Mosaico AMOLED 屏幕上的 3D 表情与文字提示（emotion 可选 neutral/happy/curious/listening/thinking/speaking/surprised/sleep）",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "emotion": { "type": "string", "description": "表情名称" },
                    "text": { "type": "string", "description": "屏幕显示文字" }
                }
            }
        }));
    }
    if has("device.audio_set_volume") {
        tools.push(json!({
            "name": "device.audio_set_volume",
            "description": "调节 Mosaico 扬声器输出音量（0 到 100）",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "level": { "type": "integer", "minimum": 0, "maximum": 100, "description": "音量百分比 0-100" }
                },
                "required": ["level"]
            }
        }));
    }
    if has("device.sensor_read") {
        tools.push(json!({
            "name": "device.sensor_read",
            "description": "读取 Mosaico 板载传感器（battery 电量%、battery_voltage 电压mV、battery_current 电流mA、temperature 温度°C、imu/accel_x/accel_y/accel_z/gyro_x/gyro_y/gyro_z）",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sensor_type": { "type": "string", "description": "传感器类型，如 battery / temperature / imu" }
                },
                "required": ["sensor_type"]
            }
        }));
    }
    if has("device.status_read") {
        tools.push(json!({
            "name": "device.status_read",
            "description": "读取 Mosaico 设备实时状态汇总（电量、温度、音量、Wi-Fi 信号、片内空闲内存、端侧声纹版本）",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }));
    }
    if has("device.actuator_set") {
        tools.push(json!({
            "name": "device.actuator_set",
            "description": "控制终端 GPIO / LED 等本地执行器",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pin": { "type": "integer", "description": "GPIO 编号" },
                    "level": { "type": "integer", "description": "输出电平 0 或 1" },
                    "action": { "type": "string", "description": "执行动作，如 set" }
                },
                "required": ["pin", "level"]
            }
        }));
    }
    if has("device.voiceprint_list") {
        tools.push(json!({
            "name": "device.voiceprint_list",
            "description": "查询 Mosaico 加密 NVS 中已同步的端侧声纹列表与版本号",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }));
    }
    if hardware
        .capabilities
        .iter()
        .any(|c| canonical_capability(c) == "device.camera_capture")
    {
        tools.push(json!({
            "name": "device.camera_capture",
            "description": "调用终端摄像头拍照",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "resolution": { "type": "string", "description": "分辨率，如 qvga / vga" }
                }
            }
        }));
    }
    tools
}

fn default_action_for_capability(cap: &str) -> &'static str {
    match canonical_capability(cap) {
        "device.display_show" => "show",
        "device.audio_set_volume" => "set_volume",
        "device.sensor_read" => "battery",
        "device.actuator_set" => "set",
        "device.status_read" => "read",
        "device.voiceprint_list" => "list",
        "device.voiceprint_delete" => "delete",
        "device.camera_capture" => "capture",
        _ => "execute",
    }
}

fn rpc_result(id: Value, result: Value) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        })),
    )
        .into_response()
}

fn rpc_error(id: Value, code: i32, message: &str) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message,
            }
        })),
    )
        .into_response()
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "ok": false, "error": message }))).into_response()
}

fn non_empty_or(value: String, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn percent_decode_segment(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).trim().to_owned()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn random_hex_short() -> String {
    let mut buf = [0_u8; 6];
    let _ = getrandom::fill(&mut buf);
    hex_bytes(&buf)
}

/// 将前端上传的 Base64 WAV 或原始 PCM16 解码为 16 kHz 单声道 PCM16 字节与样本切片。
pub(super) fn decode_audio_base64_to_pcm16(
    audio_base64: &str,
) -> Result<(Vec<u8>, Vec<i16>), String> {
    let cleaned = audio_base64
        .trim()
        .strip_prefix("data:audio/wav;base64,")
        .or_else(|| audio_base64.trim().strip_prefix("data:audio/pcm;base64,"))
        .unwrap_or_else(|| audio_base64.trim());
    let raw = BASE64
        .decode(cleaned)
        .map_err(|_| "音频 Base64 编码无效。".to_owned())?;
    if raw.is_empty() || raw.len() > MAX_AUDIO_DECODED_BYTES {
        return Err("音频体积为空或超过 4 MiB 上限。".to_owned());
    }
    let pcm_bytes = if raw.len() >= 12 && &raw[0..4] == b"RIFF" && &raw[8..12] == b"WAVE" {
        wav_bytes_to_pcm16_16k_mono(&raw).map_err(|_| "WAV 音频格式不受支持（需 PCM16）。".to_owned())?
    } else if raw.len().is_multiple_of(2) {
        raw
    } else {
        return Err("原始 PCM 字节长度必须为偶数。".to_owned());
    };
    let samples = pcm_bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    Ok((pcm_bytes, samples))
}

/// 与 ESP32-S31 固件 `FbankExtractor` + `VoiceprintEngine::ComputePooledFbankEmbedding`
/// 数值完全一致的 192 维 L2 归一化声纹特征提取器。
pub(super) fn extract_fbank_embedding_192(pcm: &[i16]) -> Option<Vec<f32>> {
    if pcm.len() < FBANK_WIN_SAMPLES {
        return None;
    }
    let num_frames = 1 + (pcm.len() - FBANK_WIN_SAMPLES) / FBANK_HOP_SAMPLES;
    if num_frames == 0 {
        return None;
    }

    let mut window = vec![0.0_f32; FBANK_WIN_SAMPLES];
    for (i, w) in window.iter_mut().enumerate() {
        *w = 0.54 - 0.46 * (2.0 * PI * (i as f32) / ((FBANK_WIN_SAMPLES - 1) as f32)).cos();
    }

    let mel_min = hz_to_mel(FBANK_F_MIN_HZ);
    let mel_max = hz_to_mel(FBANK_F_MAX_HZ.min((FBANK_SAMPLE_RATE as f32) * 0.5));
    let num_points = FBANK_NUM_MEL_BINS + 2;
    let mut fft_bins = vec![0_usize; num_points];
    for (i, bin_slot) in fft_bins.iter_mut().enumerate() {
        let mel = mel_min + (mel_max - mel_min) * (i as f32) / ((num_points - 1) as f32);
        let hz = mel_to_hz(mel);
        let bin = (((FBANK_FFT_SIZE + 1) as f32) * hz / (FBANK_SAMPLE_RATE as f32)).floor() as usize;
        *bin_slot = bin.min(FBANK_FFT_SIZE / 2);
    }

    let mut filters: Vec<(usize, Vec<f32>)> = Vec::with_capacity(FBANK_NUM_MEL_BINS);
    for m in 0..FBANK_NUM_MEL_BINS {
        let left = fft_bins[m];
        let center = fft_bins[m + 1].max(left + 1);
        let right = fft_bins[m + 2].max(center + 1);
        let capped_right = right.min(FBANK_FFT_SIZE / 2);
        let mut weights = vec![0.0_f32; capped_right - left + 1];
        for k in left..=capped_right {
            let w = if k < center {
                (k - left) as f32 / (center - left) as f32
            } else if k == center {
                1.0
            } else if right > center {
                (right - k) as f32 / (right - center) as f32
            } else {
                0.0
            };
            weights[k - left] = w.max(0.0);
        }
        filters.push((left, weights));
    }

    let spec_bins = FBANK_FFT_SIZE / 2 + 1;
    let mut fbank = vec![0.0_f32; num_frames * FBANK_NUM_MEL_BINS];
    let mut fft_buf = vec![0.0_f32; FBANK_FFT_SIZE * 2];
    let mut power = vec![0.0_f32; spec_bins];

    for f in 0..num_frames {
        let start = f * FBANK_HOP_SAMPLES;
        fft_buf.fill(0.0);
        for i in 0..FBANK_WIN_SAMPLES {
            let cur = f32::from(pcm[start + i]) / 32768.0;
            let prev = if start + i > 0 {
                f32::from(pcm[start + i - 1]) / 32768.0
            } else {
                0.0
            };
            fft_buf[2 * i] = (cur - FBANK_PRE_EMPHASIS * prev) * window[i];
        }
        radix2_fft_complex(&mut fft_buf, FBANK_FFT_SIZE);
        for k in 0..spec_bins {
            let re = fft_buf[2 * k];
            let im = fft_buf[2 * k + 1];
            power[k] = re * re + im * im;
        }
        let dst_row = &mut fbank[f * FBANK_NUM_MEL_BINS..(f + 1) * FBANK_NUM_MEL_BINS];
        for (m, (start_bin, weights)) in filters.iter().enumerate() {
            let mut energy = 1e-6_f32;
            for (j, &w) in weights.iter().enumerate() {
                let k = start_bin + j;
                if k < spec_bins {
                    energy += w * power[k];
                }
            }
            dst_row[m] = energy.ln();
        }
    }

    // CMVN（与固件 `cfg_.apply_cmvn == true` 一致）
    if num_frames > 1 {
        for m in 0..FBANK_NUM_MEL_BINS {
            let mut sum = 0.0_f64;
            for f in 0..num_frames {
                sum += f64::from(fbank[f * FBANK_NUM_MEL_BINS + m]);
            }
            let mean = (sum / (num_frames as f64)) as f32;
            for f in 0..num_frames {
                fbank[f * FBANK_NUM_MEL_BINS + m] -= mean;
            }
        }
    }

    let bins = FBANK_NUM_MEL_BINS;
    let mut mean = vec![0.0_f32; bins];
    let mut stddev = vec![0.0_f32; bins];
    let mut delta_rms = vec![0.0_f32; bins];
    let mut spectral_contrast = vec![0.0_f32; bins];

    for f in 0..num_frames {
        let row = &fbank[f * bins..(f + 1) * bins];
        for m in 0..bins {
            mean[m] += row[m];
        }
    }
    for m in 0..bins {
        mean[m] /= num_frames as f32;
    }

    for f in 0..num_frames {
        let row = &fbank[f * bins..(f + 1) * bins];
        for m in 0..bins {
            let diff = row[m] - mean[m];
            stddev[m] += diff * diff;
            if f > 0 {
                let d = row[m] - fbank[(f - 1) * bins + m];
                delta_rms[m] += d * d;
            }
            if m + 1 < bins {
                spectral_contrast[m] += row[m + 1] - row[m];
            }
        }
    }
    for m in 0..bins {
        stddev[m] = (stddev[m] / (num_frames as f32)).sqrt();
        delta_rms[m] = if num_frames > 1 {
            (delta_rms[m] / ((num_frames - 1) as f32)).sqrt()
        } else {
            0.0
        };
        spectral_contrast[m] /= num_frames as f32;
    }

    let mut out = vec![0.0_f32; EMBEDDING_DIM];
    for (i, slot) in out.iter_mut().enumerate() {
        let band = i % bins;
        let grp = i / bins;
        *slot = match grp {
            0 => stddev[band],
            1 => delta_rms[band],
            2 => spectral_contrast[band],
            3 => stddev[band] - stddev[(band + bins / 2) % bins],
            _ => delta_rms[band] * 0.5 + spectral_contrast[(band + 3) % bins] * 0.5,
        };
    }
    l2_normalize(&mut out);
    Some(out)
}

pub(super) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let dx = f64::from(x);
        let dy = f64::from(y);
        dot += dx * dy;
        na += dx * dx;
        nb += dy * dy;
    }
    if na <= 1e-12 || nb <= 1e-12 {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())) as f32
}

fn l2_normalize(vec: &mut [f32]) {
    let mut norm_sq = 0.0_f64;
    for &v in vec.iter() {
        let dv = f64::from(v);
        norm_sq += dv * dv;
    }
    if norm_sq <= 1e-12 {
        return;
    }
    let inv = (1.0 / norm_sq.sqrt()) as f32;
    for v in vec.iter_mut() {
        *v *= inv;
    }
}

fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10.0_f32.powf(mel / 2595.0) - 1.0)
}

fn radix2_fft_complex(data: &mut [f32], n: usize) {
    let mut j = 0_usize;
    for i in 0..n {
        if i < j {
            data.swap(2 * i, 2 * j);
            data.swap(2 * i + 1, 2 * j + 1);
        }
        let mut bit = n >> 1;
        while bit > 0 && (j & bit) != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
    }
    let mut len = 2_usize;
    while len <= n {
        let angle = -2.0 * PI / (len as f32);
        let wlen_re = angle.cos();
        let wlen_im = angle.sin();
        let mut i = 0;
        while i < n {
            let mut w_re = 1.0_f32;
            let mut w_im = 0.0_f32;
            for k in 0..(len / 2) {
                let even = 2 * (i + k);
                let odd = 2 * (i + k + len / 2);
                let u_re = data[even];
                let u_im = data[even + 1];
                let v_re = data[odd] * w_re - data[odd + 1] * w_im;
                let v_im = data[odd] * w_im + data[odd + 1] * w_re;
                data[even] = u_re + v_re;
                data[even + 1] = u_im + v_im;
                data[odd] = u_re - v_re;
                data[odd + 1] = u_im - v_im;
                let next_w_re = w_re * wlen_re - w_im * wlen_im;
                w_im = w_re * wlen_im + w_im * wlen_re;
                w_re = next_w_re;
            }
            i += len;
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn synth_sine_pcm(freq_hz: f32, duration_ms: usize) -> Vec<i16> {
        let total = FBANK_SAMPLE_RATE * duration_ms / 1000;
        (0..total)
            .map(|i| {
                let t = (i as f32) / (FBANK_SAMPLE_RATE as f32);
                let amp = (2.0 * PI * freq_hz * t).sin() * 0.55
                    + (2.0 * PI * freq_hz * 2.0 * t).sin() * 0.25;
                (amp * 24_000.0) as i16
            })
            .collect()
    }

    #[test]
    fn fbank_embedding_is_deterministic_normalized_and_discriminates_tones() {
        let a1 = synth_sine_pcm(180.0, 1200);
        let a2 = synth_sine_pcm(180.0, 1200);
        let b = synth_sine_pcm(620.0, 1200);
        let emb_a1 = extract_fbank_embedding_192(&a1).expect("emb a1");
        let emb_a2 = extract_fbank_embedding_192(&a2).expect("emb a2");
        let emb_b = extract_fbank_embedding_192(&b).expect("emb b");
        assert_eq!(emb_a1.len(), EMBEDDING_DIM);
        let norm: f32 = emb_a1.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm={norm}");
        assert!((cosine_similarity(&emb_a1, &emb_a2) - 1.0).abs() < 1e-5);
        assert!(cosine_similarity(&emb_a1, &emb_b) < 0.85);
    }

    #[tokio::test]
    async fn acl_store_enforces_explicit_deny_and_speaker_role_rank() {
        let dir = tempdir().expect("tempdir");
        let store = NativeConsoleStore::load_or_default(dir.path());
        let hw = DeviceHardwareDeclaration {
            mac: "30:ed:a0:f4:5b:f0".to_owned(),
            chip: "esp32s31".to_owned(),
            model: "esp32s31-mosaico".to_owned(),
            zone: "desk".to_owned(),
            group_id: "home-default".to_owned(),
            vp_version: 0,
            multinet_ready: true,
            wake_config: None,
            capabilities: vec!["device.display_show".to_owned(), "device.actuator_set".to_owned()],
        };

        // 默认种子允许 ez-assistant Owner 调用全部能力，拒绝 Guest。
        assert!(
            store
                .check_access("ez-assistant", 1, &hw, "dev-1", "device.display_show")
                .await
                .is_ok()
        );
        assert!(
            store
                .check_access("ez-assistant", 3, &hw, "dev-1", "device.display_show")
                .await
                .is_err()
        );

        // 显式拒绝规则优先级高于允许规则。
        store
            .upsert_acl(NativeAclRule {
                rule_id: "deny-gpio".to_owned(),
                agent_id: "ez-assistant".to_owned(),
                group_id: "*".to_owned(),
                zone_pattern: "*".to_owned(),
                device_id: "*".to_owned(),
                capability_pattern: "device.actuator_*".to_owned(),
                min_speaker_role: 1,
                allow: false,
            })
            .await
            .expect("save deny rule");
        assert!(
            store
                .check_access("ez-assistant", 1, &hw, "dev-1", "device.actuator_set")
                .await
                .is_err()
        );
        assert!(
            store
                .check_access("ez-assistant", 1, &hw, "dev-1", "gpio.set")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn voiceprint_store_persists_enrolls_identifies_and_reloads() {
        let dir = tempdir().expect("tempdir");
        let store = NativeConsoleStore::load_or_default(dir.path());
        let emb = extract_fbank_embedding_192(&synth_sine_pcm(210.0, 1200)).expect("emb");
        let (_, sync) = store
            .enroll_speaker(
                NativeSpeakerRecord {
                    speaker_id: "spk_owner".to_owned(),
                    display_name: "Luke".to_owned(),
                    uid: "u_1".to_owned(),
                    agent_id: "ez-assistant".to_owned(),
                    group_id: "home-default".to_owned(),
                    role: 1,
                    tts_voice: "stella".to_owned(),
                    prompt: "请称呼我主人".to_owned(),
                    embedding: Vec::new(),
                    templates: Vec::new(),
                    version: 0,
                },
                emb.clone(),
            )
            .await
            .expect("enroll");
        assert_eq!(sync.vp_version, 1);
        assert_eq!(sync.upserts.len(), 1);

        let reloaded = NativeConsoleStore::load_or_default(dir.path());
        let (matched, best, score) = reloaded.identify_embedding(&emb).await;
        assert!(matched);
        assert_eq!(best.expect("best").speaker_id, "spk_owner");
        assert!(score > 0.99);
    }

    #[tokio::test]
    async fn prepare_initial_sync_raises_floor_above_device_version_and_skips_redundant_writes() {
        let dir = tempdir().expect("tempdir");
        let store = NativeConsoleStore::load_or_default(dir.path());
        // Fresh host sees device with pre-existing NVS vp_version=4 -> raises floor without empty flash write
        assert!(store.prepare_initial_sync(4).await.is_none());
        assert_eq!(store.vp_version().await, 4);

        // Next enroll gets vp_version=5 (> 4) so device ApplySync accepts it immediately
        let emb = extract_fbank_embedding_192(&synth_sine_pcm(260.0, 1200)).expect("emb");
        let (_, sync) = store
            .enroll_speaker(
                NativeSpeakerRecord {
                    speaker_id: "spk_new".to_owned(),
                    display_name: "New".to_owned(),
                    uid: "u_2".to_owned(),
                    agent_id: "ez-assistant".to_owned(),
                    group_id: "home-default".to_owned(),
                    role: 1,
                    tts_voice: String::new(),
                    prompt: String::new(),
                    embedding: Vec::new(),
                    templates: Vec::new(),
                    version: 0,
                },
                emb,
            )
            .await
            .expect("enroll");
        assert_eq!(sync.vp_version, 5);

        // Reconnect at matching version skips redundant NVS sync
        assert!(store.prepare_initial_sync(5).await.is_none());
        // Reconnect at stale device version pushes full sync at v5
        let catchup = store
            .prepare_initial_sync(4)
            .await
            .expect("catchup sync");
        assert_eq!(catchup.vp_version, 5);
        assert_eq!(catchup.upserts.len(), 1);
    }

    #[tokio::test]
    async fn wake_config_enforces_voiceprint_guardrail_multi_template_and_auto_fallback() {
        let dir = tempdir().expect("tempdir");
        let store = NativeConsoleStore::load_or_default(dir.path());

        let (initial, count) = store.wake_config().await;
        assert_eq!(count, 0);
        assert_eq!(initial.mode, WakeMode::WakeWord);
        assert_eq!(initial.wake_word, "你好小元");
        assert_eq!(initial.wake_pinyin, "ni hao xiao yuan");

        // Cannot enable voiceprint_direct without enrolled speakers
        let err = store
            .update_wake_config(DeviceWakeConfig {
                mode: WakeMode::VoiceprintDirect,
                ..DeviceWakeConfig::default()
            })
            .await
            .expect_err("must require enrolled speaker");
        assert!(err.contains("声纹"));

        // Invalid MultiNet7 syllable is rejected
        assert!(
            store
                .update_wake_config(DeviceWakeConfig {
                    wake_word: "测试".to_owned(),
                    wake_pinyin: "ce xyz".to_owned(),
                    ..DeviceWakeConfig::default()
                })
                .await
                .is_err()
        );

        // Multi-template enroll (3 samples) -> emits centroid + #2 + #3
        let e1 = extract_fbank_embedding_192(&synth_sine_pcm(180.0, 1200)).expect("e1");
        let e2 = extract_fbank_embedding_192(&synth_sine_pcm(205.0, 1200)).expect("e2");
        let e3 = extract_fbank_embedding_192(&synth_sine_pcm(230.0, 1200)).expect("e3");
        let (_, vp_sync) = store
            .enroll_speaker_multi(
                NativeSpeakerRecord {
                    speaker_id: "spk_owner".to_owned(),
                    display_name: "Owner".to_owned(),
                    uid: "u_1".to_owned(),
                    agent_id: "ez-assistant".to_owned(),
                    group_id: "home-default".to_owned(),
                    role: 1,
                    tts_voice: String::new(),
                    prompt: String::new(),
                    embedding: Vec::new(),
                    templates: Vec::new(),
                    version: 0,
                },
                vec![e1.clone(), e2, e3],
            )
            .await
            .expect("multi enroll");
        assert_eq!(vp_sync.upserts.len(), 3);
        assert_eq!(vp_sync.upserts[0].speaker_id, "spk_owner");
        assert_eq!(vp_sync.upserts[1].speaker_id, "spk_owner#2");
        assert_eq!(vp_sync.upserts[2].speaker_id, "spk_owner#3");

        // Now switching to voiceprint_direct + custom wake word succeeds
        let (saved, _, enrolled) = store
            .update_wake_config(DeviceWakeConfig {
                mode: WakeMode::VoiceprintDirect,
                wake_word: "小元同学".to_owned(),
                wake_pinyin: "Xiao Yuan Tong Xue".to_owned(),
                vp_threshold: 0.80,
                ..DeviceWakeConfig::default()
            })
            .await
            .expect("enable voiceprint_direct");
        assert_eq!(enrolled, 1);
        assert_eq!(saved.mode, WakeMode::VoiceprintDirect);
        assert_eq!(saved.wake_pinyin, "xiao yuan tong xue");

        // Deleting the last speaker automatically falls back to wake_word mode
        let (deleted, _, fallback) = store.delete_speaker("spk_owner").await.expect("delete");
        assert!(deleted);
        let fallback_sync = fallback.expect("must auto fallback to wake_word");
        assert_eq!(fallback_sync.config.mode, WakeMode::WakeWord);
        assert_eq!(fallback_sync.enrolled_speaker_count, 0);
        let (after_cfg, after_cnt) = store.wake_config().await;
        assert_eq!(after_cnt, 0);
        assert_eq!(after_cfg.mode, WakeMode::WakeWord);
    }
}
