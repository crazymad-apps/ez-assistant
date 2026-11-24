//! Provider-neutral ASR/TTS 端口及 DashScope、OpenAI 兼容 Adapter。

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use reqwest::{Client, StatusCode, Url};
use serde_json::{Value, json};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

const MAX_PROVIDER_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_TTS_PCM_BYTES: usize = 60 * 16_000 * 2;
const MAX_TTS_WAV_DOWNLOAD_BYTES: usize = MAX_TTS_PCM_BYTES * 6;
const TARGET_SAMPLE_RATE_HZ: u32 = 16_000;

/// Provider-neutral 的整句 ASR 输入；音频固定为 16 kHz 单声道 PCM16 little-endian。
pub(super) struct AsrRequest {
    pub(super) pcm_s16le: Arc<[u8]>,
    pub(super) cancellation: CancellationToken,
}

/// ASR Provider 完成后返回的非空最终转写。
pub(super) struct AsrResult {
    pub(super) transcript: String,
}

/// 可借用 Provider 和请求的 ASR 异步调用结果。
pub(super) type AsrFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AsrResult, SpeechProviderError>> + Send + 'a>>;

/// TTS Provider 返回的 16 kHz 单声道 PCM16 little-endian 音频。
pub(super) struct TtsResult {
    pub(super) pcm_s16le: Arc<[u8]>,
}

/// 可借用 Provider 输入的 TTS 异步调用结果。
pub(super) type TtsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<TtsResult, SpeechProviderError>> + Send + 'a>>;

/// 可替换的整句语音识别端口。
pub(super) trait AsrProvider: Send + Sync {
    fn recognize<'a>(&'a self, request: &'a AsrRequest) -> AsrFuture<'a>;
}

/// 可替换的短文本语音合成端口。
pub(super) trait TtsProvider: Send + Sync {
    fn synthesize<'a>(&'a self, text: &'a str, cancellation: CancellationToken) -> TtsFuture<'a>;
}

/// 阿里百炼整句识别 Adapter；只持有已校验端点和私有凭据。
#[derive(Clone)]
pub(super) struct DashScopeSpeechRecognizer {
    client: Client,
    endpoint: Url,
    model: Arc<str>,
    credential: Arc<str>,
}

impl DashScopeSpeechRecognizer {
    pub(super) fn new(
        endpoint: String,
        model: String,
        credential: String,
        timeout: Duration,
    ) -> Result<Self, SpeechProviderError> {
        let mut endpoint = Url::parse(endpoint.trim_end_matches('/'))
            .map_err(|_| SpeechProviderError::Unavailable)?;
        let allowed = endpoint.scheme() == "https"
            || (endpoint.scheme() == "http"
                && endpoint
                    .host_str()
                    .is_some_and(|host| host == "127.0.0.1" || host == "localhost"));
        if !allowed {
            return Err(SpeechProviderError::Unavailable);
        }
        endpoint.set_path("/api/v1/services/aigc/multimodal-generation/generation");
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|_| SpeechProviderError::Unavailable)?;
        Ok(Self {
            client,
            endpoint,
            model: Arc::from(model),
            credential: Arc::from(credential),
        })
    }
}

impl AsrProvider for DashScopeSpeechRecognizer {
    fn recognize<'a>(&'a self, request: &'a AsrRequest) -> AsrFuture<'a> {
        Box::pin(async move {
            let wav = pcm16_mono_wav(&request.pcm_s16le)?;
            let audio = format!("data:audio/wav;base64,{}", BASE64.encode(wav));
            let payload = json!({
                "model": self.model.as_ref(),
                "input": {"messages": [{
                    "role": "user",
                    "content": [{"type": "input_audio", "input_audio": {"data": audio}}]
                }]},
                "parameters": {"format": "wav", "sample_rate": "16000"}
            });
            let send = self
                .client
                .post(self.endpoint.clone())
                .bearer_auth(self.credential.as_ref())
                .header("X-DashScope-SSE", "disable")
                .json(&payload)
                .send();
            let response = tokio::select! {
                () = request.cancellation.cancelled() => return Err(SpeechProviderError::Cancelled),
                response = send => response.map_err(map_transport)?,
            };
            let status = response.status();
            if !status.is_success() {
                return Err(map_status(status));
            }
            let body = bounded_json(response, &request.cancellation).await?;
            let transcript = ["/output/text", "/output/output/text"]
                .iter()
                .find_map(|pointer| body.pointer(pointer).and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToOwned::to_owned)
                .ok_or(SpeechProviderError::InvalidTranscript)?;
            Ok(AsrResult { transcript })
        })
    }
}

/// 阿里百炼语音合成 Adapter；下载地址仍需通过受信 Host 白名单校验。
#[derive(Clone)]
pub(super) struct DashScopeSpeechSynthesizer {
    client: Client,
    endpoint: Url,
    model: Arc<str>,
    voice: Arc<str>,
    credential: Arc<str>,
}

impl DashScopeSpeechSynthesizer {
    pub(super) fn new(
        endpoint: String,
        model: String,
        voice: String,
        credential: String,
        timeout: Duration,
    ) -> Result<Self, SpeechProviderError> {
        let mut endpoint = Url::parse(endpoint.trim_end_matches('/'))
            .map_err(|_| SpeechProviderError::Unavailable)?;
        if !is_secure_or_loopback(&endpoint) {
            return Err(SpeechProviderError::Unavailable);
        }
        endpoint.set_path("/api/v1/services/audio/tts/SpeechSynthesizer");
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|_| SpeechProviderError::Unavailable)?;
        Ok(Self {
            client,
            endpoint,
            model: Arc::from(model),
            voice: Arc::from(voice),
            credential: Arc::from(credential),
        })
    }
}

impl TtsProvider for DashScopeSpeechSynthesizer {
    fn synthesize<'a>(&'a self, text: &'a str, cancellation: CancellationToken) -> TtsFuture<'a> {
        Box::pin(async move {
            let payload = json!({
                "model": self.model.as_ref(),
                "input": {
                    "text": text,
                    "voice": self.voice.as_ref(),
                    "format": "pcm",
                    "sample_rate": 16000,
                    "language_hints": ["zh"]
                }
            });
            let send = self
                .client
                .post(self.endpoint.clone())
                .bearer_auth(self.credential.as_ref())
                .json(&payload)
                .send();
            let response = tokio::select! {
                () = cancellation.cancelled() => return Err(SpeechProviderError::Cancelled),
                response = send => response.map_err(map_transport)?,
            };
            let status = response.status();
            if !status.is_success() {
                return Err(map_status(status));
            }
            let body = bounded_json(response, &cancellation).await?;
            let audio_url = body
                .pointer("/output/audio/url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or(SpeechProviderError::InvalidAudio)?;
            let audio_url = secure_audio_url(audio_url)?;
            let download = self.client.get(audio_url).send();
            let response = tokio::select! {
                () = cancellation.cancelled() => return Err(SpeechProviderError::Cancelled),
                response = download => response.map_err(map_transport)?,
            };
            let status = response.status();
            if !status.is_success() {
                return Err(map_status(status));
            }
            let pcm = bounded_bytes(response, &cancellation, MAX_TTS_PCM_BYTES).await?;
            if pcm.is_empty() || !pcm.len().is_multiple_of(2) {
                return Err(SpeechProviderError::InvalidAudio);
            }
            Ok(TtsResult {
                pcm_s16le: Arc::from(pcm),
            })
        })
    }
}

/// Provider Adapter 内部的稳定失败分类。
///
/// 上层会再次映射为 SpeechService 错误，任何服务端响应正文都不外泄。
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(super) enum SpeechProviderError {
    #[error("speech service is unavailable")]
    Unavailable,
    #[error("speech provider authentication failed")]
    Authentication,
    #[error("speech provider request timed out")]
    Timeout,
    #[error("speech provider request failed")]
    ProviderFailed,
    #[error("speech provider returned an invalid transcript")]
    InvalidTranscript,
    #[error("speech request was cancelled")]
    Cancelled,
    #[error("speech input PCM is invalid")]
    InvalidPcm,
    #[error("speech provider returned invalid audio")]
    InvalidAudio,
    #[error("speech provider audio exceeded the output limit")]
    OutputTooLarge,
}

/// OpenAI 兼容语音识别 Adapter（`/v1/audio/transcriptions`）；发送 16 kHz WAV 并读取 `text`。
#[derive(Clone)]
pub(super) struct OpenAiSpeechRecognizer {
    client: Client,
    endpoint: Url,
    model: Arc<str>,
    credential: Arc<str>,
}

impl OpenAiSpeechRecognizer {
    pub(super) fn new(
        endpoint: String,
        model: String,
        credential: String,
        timeout: Duration,
    ) -> Result<Self, SpeechProviderError> {
        let endpoint = resolve_openai_endpoint(&endpoint, "audio/transcriptions")?;
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|_| SpeechProviderError::Unavailable)?;
        Ok(Self {
            client,
            endpoint,
            model: Arc::from(model),
            credential: Arc::from(credential),
        })
    }
}

impl AsrProvider for OpenAiSpeechRecognizer {
    fn recognize<'a>(&'a self, request: &'a AsrRequest) -> AsrFuture<'a> {
        Box::pin(async move {
            let wav = pcm16_mono_wav(&request.pcm_s16le)?;
            let boundary = format!("ezspeech{}", crate::media_diagnostics::timestamp_ms());
            let body = build_asr_multipart_body(&boundary, &self.model, &wav);
            let send = self
                .client
                .post(self.endpoint.clone())
                .bearer_auth(self.credential.as_ref())
                .header(
                    reqwest::header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(body)
                .send();
            let response = tokio::select! {
                () = request.cancellation.cancelled() => return Err(SpeechProviderError::Cancelled),
                response = send => response.map_err(map_transport)?,
            };
            let status = response.status();
            if !status.is_success() {
                return Err(map_status(status));
            }
            let body = bounded_json(response, &request.cancellation).await?;
            let transcript = body
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToOwned::to_owned)
                .ok_or(SpeechProviderError::InvalidTranscript)?;
            Ok(AsrResult { transcript })
        })
    }
}

/// OpenAI 兼容语音合成 Adapter（`/v1/audio/speech`）；请求 WAV 并统一重采样为 16 kHz 单声道 PCM16。
#[derive(Clone)]
pub(super) struct OpenAiSpeechSynthesizer {
    client: Client,
    endpoint: Url,
    model: Arc<str>,
    voice: Arc<str>,
    credential: Arc<str>,
}

impl OpenAiSpeechSynthesizer {
    pub(super) fn new(
        endpoint: String,
        model: String,
        voice: String,
        credential: String,
        timeout: Duration,
    ) -> Result<Self, SpeechProviderError> {
        let endpoint = resolve_openai_endpoint(&endpoint, "audio/speech")?;
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|_| SpeechProviderError::Unavailable)?;
        Ok(Self {
            client,
            endpoint,
            model: Arc::from(model),
            voice: Arc::from(voice),
            credential: Arc::from(credential),
        })
    }
}

impl TtsProvider for OpenAiSpeechSynthesizer {
    fn synthesize<'a>(&'a self, text: &'a str, cancellation: CancellationToken) -> TtsFuture<'a> {
        Box::pin(async move {
            let payload = json!({
                "model": self.model.as_ref(),
                "input": text,
                "voice": self.voice.as_ref(),
                "response_format": "wav",
            });
            let send = self
                .client
                .post(self.endpoint.clone())
                .bearer_auth(self.credential.as_ref())
                .json(&payload)
                .send();
            let response = tokio::select! {
                () = cancellation.cancelled() => return Err(SpeechProviderError::Cancelled),
                response = send => response.map_err(map_transport)?,
            };
            let status = response.status();
            if !status.is_success() {
                return Err(map_status(status));
            }
            let raw = bounded_bytes(response, &cancellation, MAX_TTS_WAV_DOWNLOAD_BYTES).await?;
            let pcm = wav_bytes_to_pcm16_16k_mono(&raw)?;
            if pcm.len() > MAX_TTS_PCM_BYTES {
                return Err(SpeechProviderError::OutputTooLarge);
            }
            Ok(TtsResult {
                pcm_s16le: Arc::from(pcm),
            })
        })
    }
}

fn resolve_openai_endpoint(raw: &str, suffix: &str) -> Result<Url, SpeechProviderError> {
    let mut url = Url::parse(raw.trim().trim_end_matches('/'))
        .map_err(|_| SpeechProviderError::Unavailable)?;
    if !is_secure_or_loopback(&url) {
        return Err(SpeechProviderError::Unavailable);
    }
    let path = url.path().trim_end_matches('/');
    let next_path = if path.ends_with(&format!("/{suffix}")) {
        path.to_owned()
    } else if path.is_empty() || path == "/" {
        format!("/v1/{suffix}")
    } else if path.ends_with("/v1") {
        format!("{path}/{suffix}")
    } else {
        format!("{path}/v1/{suffix}")
    };
    url.set_path(&next_path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn build_asr_multipart_body(boundary: &str, model: &str, wav: &[u8]) -> Vec<u8> {
    let clean_model = model.replace(['\r', '\n', '"'], "");
    let mut body = Vec::with_capacity(wav.len() + 256);
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
    body.extend_from_slice(clean_model.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

fn wav_bytes_to_pcm16_16k_mono(wav: &[u8]) -> Result<Vec<u8>, SpeechProviderError> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err(SpeechProviderError::InvalidAudio);
    }
    let mut pos = 12_usize;
    let mut channels = 0_u16;
    let mut sample_rate = 0_u32;
    let mut bits_per_sample = 0_u16;
    let mut audio_format = 0_u16;
    let mut found_fmt = false;
    let mut data_slice: Option<&[u8]> = None;

    while pos.saturating_add(8) <= wav.len() {
        let id = &wav[pos..pos + 4];
        let chunk_len =
            u32::from_le_bytes([wav[pos + 4], wav[pos + 5], wav[pos + 6], wav[pos + 7]]) as usize;
        pos += 8;
        let Some(end) = pos.checked_add(chunk_len) else {
            return Err(SpeechProviderError::InvalidAudio);
        };
        if end > wav.len() {
            return Err(SpeechProviderError::InvalidAudio);
        }
        let chunk = &wav[pos..end];
        if id == b"fmt " {
            if chunk.len() < 16 {
                return Err(SpeechProviderError::InvalidAudio);
            }
            audio_format = u16::from_le_bytes([chunk[0], chunk[1]]);
            channels = u16::from_le_bytes([chunk[2], chunk[3]]);
            sample_rate = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
            bits_per_sample = u16::from_le_bytes([chunk[14], chunk[15]]);
            found_fmt = true;
        } else if id == b"data" {
            data_slice = Some(chunk);
            break;
        }
        // RIFF chunk 按偶数字节对齐。
        pos = end.saturating_add(chunk_len % 2);
    }

    let Some(data) = data_slice else {
        return Err(SpeechProviderError::InvalidAudio);
    };
    if !found_fmt
        || audio_format != 1
        || !matches!(channels, 1 | 2)
        || sample_rate == 0
        || bits_per_sample != 16
        || data.is_empty()
    {
        return Err(SpeechProviderError::InvalidAudio);
    }
    let frame_bytes = usize::from(channels) * 2;
    let frame_count = data.len() / frame_bytes;
    if frame_count == 0 {
        return Err(SpeechProviderError::InvalidAudio);
    }

    let mut mono = Vec::with_capacity(frame_count);
    for frame in 0..frame_count {
        let offset = frame * frame_bytes;
        let left = i32::from(i16::from_le_bytes([data[offset], data[offset + 1]]));
        let sample = if channels == 2 {
            let right = i32::from(i16::from_le_bytes([data[offset + 2], data[offset + 3]]));
            ((left + right) / 2) as i16
        } else {
            left as i16
        };
        mono.push(sample);
    }

    if sample_rate == TARGET_SAMPLE_RATE_HZ {
        let mut out = Vec::with_capacity(mono.len() * 2);
        for s in mono {
            out.extend_from_slice(&s.to_le_bytes());
        }
        return Ok(out);
    }

    let out_len_u64 = (mono.len() as u64).saturating_mul(u64::from(TARGET_SAMPLE_RATE_HZ))
        / u64::from(sample_rate);
    let out_len =
        usize::try_from(out_len_u64.max(1)).map_err(|_| SpeechProviderError::OutputTooLarge)?;
    if out_len.saturating_mul(2) > MAX_TTS_PCM_BYTES {
        return Err(SpeechProviderError::OutputTooLarge);
    }
    let mut out = Vec::with_capacity(out_len * 2);
    let step = sample_rate as f64 / f64::from(TARGET_SAMPLE_RATE_HZ);
    for i in 0..out_len {
        let pos = i as f64 * step;
        let idx = (pos.floor() as usize).min(mono.len() - 1);
        let frac = pos - idx as f64;
        let s0 = f64::from(mono[idx]);
        let s1 = f64::from(mono[(idx + 1).min(mono.len() - 1)]);
        let interpolated = (s0 + frac * (s1 - s0)).round().clamp(-32768.0, 32767.0) as i16;
        out.extend_from_slice(&interpolated.to_le_bytes());
    }
    Ok(out)
}

fn map_transport(error: reqwest::Error) -> SpeechProviderError {
    if error.is_timeout() {
        SpeechProviderError::Timeout
    } else {
        SpeechProviderError::ProviderFailed
    }
}

fn map_status(status: StatusCode) -> SpeechProviderError {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        SpeechProviderError::Authentication
    } else if status == StatusCode::REQUEST_TIMEOUT {
        SpeechProviderError::Timeout
    } else {
        SpeechProviderError::ProviderFailed
    }
}

async fn bounded_json(
    mut response: reqwest::Response,
    cancellation: &CancellationToken,
) -> Result<Value, SpeechProviderError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PROVIDER_RESPONSE_BYTES as u64)
    {
        return Err(SpeechProviderError::ProviderFailed);
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::select! {
            () = cancellation.cancelled() => return Err(SpeechProviderError::Cancelled),
            chunk = response.chunk() => chunk.map_err(map_transport)?,
        };
        let Some(chunk) = chunk else { break };
        if bytes.len().saturating_add(chunk.len()) > MAX_PROVIDER_RESPONSE_BYTES {
            return Err(SpeechProviderError::ProviderFailed);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| SpeechProviderError::ProviderFailed)
}

async fn bounded_bytes(
    mut response: reqwest::Response,
    cancellation: &CancellationToken,
    limit: usize,
) -> Result<Vec<u8>, SpeechProviderError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(SpeechProviderError::OutputTooLarge);
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::select! {
            () = cancellation.cancelled() => return Err(SpeechProviderError::Cancelled),
            chunk = response.chunk() => chunk.map_err(map_transport)?,
        };
        let Some(chunk) = chunk else { break };
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(SpeechProviderError::OutputTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn secure_audio_url(value: &str) -> Result<Url, SpeechProviderError> {
    let mut url = Url::parse(value).map_err(|_| SpeechProviderError::InvalidAudio)?;
    let Some(host) = url.host_str() else {
        return Err(SpeechProviderError::InvalidAudio);
    };
    if host == "127.0.0.1" || host == "localhost" {
        return (url.scheme() == "http" || url.scheme() == "https")
            .then_some(url)
            .ok_or(SpeechProviderError::InvalidAudio);
    }
    if !host.ends_with(".oss-cn-beijing.aliyuncs.com") {
        return Err(SpeechProviderError::InvalidAudio);
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" => {
            url.set_scheme("https")
                .map_err(|_| SpeechProviderError::InvalidAudio)?;
            Ok(url)
        }
        _ => Err(SpeechProviderError::InvalidAudio),
    }
}

fn is_secure_or_loopback(url: &Url) -> bool {
    url.scheme() == "https"
        || (url.scheme() == "http"
            && url
                .host_str()
                .is_some_and(|host| host == "127.0.0.1" || host == "localhost"))
}

fn pcm16_mono_wav(pcm: &[u8]) -> Result<Vec<u8>, SpeechProviderError> {
    if pcm.is_empty() || !pcm.len().is_multiple_of(2) {
        return Err(SpeechProviderError::InvalidPcm);
    }
    let data_length = u32::try_from(pcm.len()).map_err(|_| SpeechProviderError::InvalidPcm)?;
    let riff_length = data_length
        .checked_add(36)
        .ok_or(SpeechProviderError::InvalidPcm)?;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_length.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&16_000_u32.to_le_bytes());
    wav.extend_from_slice(&32_000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_length.to_le_bytes());
    wav.extend_from_slice(pcm);
    Ok(wav)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Bytes,
        routing::{get, post},
    };

    #[test]
    fn pcm_is_wrapped_in_canonical_16k_mono_wav() {
        let wav = pcm16_mono_wav(&[1, 2, 3, 4]).expect("wav");
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        assert_eq!(&wav[44..], &[1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn dashscope_adapter_sends_whole_wav_and_reads_final_transcript() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let app = Router::new().route(
            "/api/v1/services/aigc/multimodal-generation/generation",
            post(|| async { Json(json!({"output": {"text": "  测试转写  "}})) }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let provider = DashScopeSpeechRecognizer::new(
            format!("http://{address}"),
            "fixture-asr".to_owned(),
            "fixture-secret".to_owned(),
            Duration::from_secs(2),
        )
        .expect("provider");
        let request = AsrRequest {
            pcm_s16le: Arc::from(vec![0_u8; 640]),
            cancellation: CancellationToken::new(),
        };
        let result = provider.recognize(&request).await.expect("ASR result");
        assert_eq!(result.transcript, "测试转写");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn cancelled_asr_does_not_start_a_provider_request() {
        let provider = DashScopeSpeechRecognizer::new(
            "http://127.0.0.1:9".to_owned(),
            "fixture-asr".to_owned(),
            "fixture-secret".to_owned(),
            Duration::from_secs(2),
        )
        .expect("provider");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let request = AsrRequest {
            pcm_s16le: Arc::from(vec![0_u8; 640]),
            cancellation,
        };
        assert!(matches!(
            provider.recognize(&request).await,
            Err(SpeechProviderError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn dashscope_tts_adapter_downloads_bounded_pcm() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let audio_url = format!("http://{address}/fixture.pcm");
        let app = Router::new()
            .route(
                "/api/v1/services/audio/tts/SpeechSynthesizer",
                post(move || {
                    let audio_url = audio_url.clone();
                    async move { Json(json!({"output": {"audio": {"url": audio_url}}})) }
                }),
            )
            .route(
                "/fixture.pcm",
                get(|| async { Bytes::from_static(&[1_u8, 2, 3, 4]) }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let provider = DashScopeSpeechSynthesizer::new(
            format!("http://{address}"),
            "fixture-tts".to_owned(),
            "fixture-voice".to_owned(),
            "fixture-secret".to_owned(),
            Duration::from_secs(2),
        )
        .expect("provider");
        let result = provider
            .synthesize("简短播报", CancellationToken::new())
            .await
            .expect("TTS result");
        assert_eq!(result.pcm_s16le.as_ref(), &[1, 2, 3, 4]);
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn tts_audio_url_rejects_untrusted_hosts() {
        assert!(matches!(
            secure_audio_url("https://example.com/audio.pcm"),
            Err(SpeechProviderError::InvalidAudio)
        ));
    }

    #[tokio::test]
    async fn openai_asr_and_tts_adapters_round_trip_wav_audio() {
        let received_header = Arc::new(std::sync::Mutex::new(String::new()));
        let captured_header = received_header.clone();
        let tts_wav = wav_with_rate_and_samples(24_000, &[1000_i16; 2400]);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let app = Router::new()
            .route(
                "/v1/audio/transcriptions",
                post(move |headers: axum::http::HeaderMap, body: Bytes| {
                    let captured = captured_header.clone();
                    async move {
                        *captured.lock().unwrap() = headers
                            .get("content-type")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_owned();
                        assert!(body.windows(4).any(|w| w == b"RIFF"));
                        Json(json!({"text": "  你好小元  "}))
                    }
                }),
            )
            .route(
                "/v1/audio/speech",
                post(move || {
                    let tts_wav = tts_wav.clone();
                    async move { Bytes::from(tts_wav) }
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let asr = OpenAiSpeechRecognizer::new(
            format!("http://{address}/v1"),
            "whisper-1".to_owned(),
            "sk-test".to_owned(),
            Duration::from_secs(2),
        )
        .expect("openai asr");
        let result = asr
            .recognize(&AsrRequest {
                pcm_s16le: Arc::from(vec![0_u8; 640]),
                cancellation: CancellationToken::new(),
            })
            .await
            .expect("asr ok");
        assert_eq!(result.transcript, "你好小元");
        assert!(
            received_header
                .lock()
                .unwrap()
                .starts_with("multipart/form-data; boundary=")
        );

        let tts = OpenAiSpeechSynthesizer::new(
            format!("http://{address}"),
            "tts-1".to_owned(),
            "alloy".to_owned(),
            "sk-test".to_owned(),
            Duration::from_secs(2),
        )
        .expect("openai tts");
        let synthesized = tts
            .synthesize("测试播报", CancellationToken::new())
            .await
            .expect("tts ok");
        // 2400 samples @ 24kHz -> 1600 samples @ 16kHz -> 3200 bytes.
        assert_eq!(synthesized.pcm_s16le.len(), 3200);

        server.abort();
        let _ = server.await;
    }

    fn wav_with_rate_and_samples(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
        let data_bytes = (samples.len() * 2) as u32;
        let mut wav = Vec::with_capacity(44 + data_bytes as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_bytes.to_le_bytes());
        for sample in samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        wav
    }
}
