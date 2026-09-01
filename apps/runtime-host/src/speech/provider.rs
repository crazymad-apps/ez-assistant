//! Provider-neutral ASR 端口与首个 DashScope 整句识别 Adapter。

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use reqwest::{Client, StatusCode, Url};
use serde_json::{Value, json};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

const MAX_PROVIDER_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_TTS_PCM_BYTES: usize = 60 * 16_000 * 2;

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
}
