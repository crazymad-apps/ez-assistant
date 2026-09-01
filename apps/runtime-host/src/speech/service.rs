//! SpeechService actor：原子交换 Provider 配置并拥有全部并发语音请求。

use std::{path::PathBuf, sync::Arc};

use assistant_protocol::{DeviceSpeechServicesSnapshot, SpeechServiceStatusSnapshot};
use assistant_runtime::RuntimeConfigSource;
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use super::{
    config,
    provider::{AsrProvider, AsrRequest, SpeechProviderError, TtsProvider},
};

const COMMAND_CAPACITY: usize = 32;

/// Host 语音能力的长期 actor，串行接收配置与请求并拥有所有请求子任务。
///
/// 关闭时会取消并回收仍在执行的 Provider 请求；ASR/TTS 状态通过 watch 投影给 Gateway。
pub(crate) struct SpeechService {
    source: Arc<dyn RuntimeConfigSource>,
    commands: mpsc::Receiver<SpeechCommand>,
    status: watch::Sender<DeviceSpeechServicesSnapshot>,
}

/// 语音 actor 的可克隆调用句柄。
///
/// 克隆句柄只持有有界命令发送端和状态订阅，不复制 Provider 或请求生命周期。
#[derive(Clone)]
pub(crate) struct SpeechServiceHandle {
    commands: mpsc::Sender<SpeechCommand>,
    status: watch::Receiver<DeviceSpeechServicesSnapshot>,
}

/// 一次整句 ASR 请求及其独立取消边界。
struct RecognizeRequest {
    pcm: Arc<[u8]>,
    debug_name: String,
    cancellation: CancellationToken,
}

/// 一次短文本 TTS 请求及其独立取消边界。
struct SynthesizeRequest {
    text: String,
    debug_name: String,
    cancellation: CancellationToken,
}

/// 语音 actor 串行接收的内部命令。
enum SpeechCommand {
    Reload {
        response: oneshot::Sender<()>,
    },
    Recognize {
        request: RecognizeRequest,
        response: oneshot::Sender<Result<String, SpeechServiceError>>,
    },
    Synthesize {
        request: SynthesizeRequest,
        response: oneshot::Sender<Result<Arc<[u8]>, SpeechServiceError>>,
    },
}

impl SpeechService {
    pub(crate) fn new(source: Arc<dyn RuntimeConfigSource>) -> (Self, SpeechServiceHandle) {
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (status_tx, status_rx) = watch::channel(DeviceSpeechServicesSnapshot::default());
        (
            Self {
                source,
                commands: command_rx,
                status: status_tx,
            },
            SpeechServiceHandle {
                commands: command_tx,
                status: status_rx,
            },
        )
    }

    pub(crate) async fn run_until(
        mut self,
        shutdown: CancellationToken,
    ) -> Result<(), SpeechServiceError> {
        let mut compiled = config::load(self.source.as_ref()).await;
        self.status.send_replace(compiled.status);
        let mut requests = JoinSet::new();
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                command = self.commands.recv() => {
                    let Some(command) = command else { break };
                    match command {
                        SpeechCommand::Reload { response } => {
                            compiled = config::load(self.source.as_ref()).await;
                            self.status.send_replace(compiled.status);
                            let _ = response.send(());
                        }
                        SpeechCommand::Recognize { request, response } => {
                            let provider = compiled.asr.clone();
                            let debug_directory = compiled.debug_audio_directory.clone();
                            let request_shutdown = shutdown.child_token();
                            requests.spawn(async move {
                                let result = recognize(provider, debug_directory, request, request_shutdown).await;
                                let _ = response.send(result);
                            });
                        }
                        SpeechCommand::Synthesize { request, response } => {
                            let provider = compiled.tts.clone();
                            let debug_directory = compiled.debug_audio_directory.clone();
                            let request_shutdown = shutdown.child_token();
                            requests.spawn(async move {
                                let result = synthesize(provider, debug_directory, request, request_shutdown).await;
                                let _ = response.send(result);
                            });
                        }
                    }
                }
                completed = requests.join_next(), if !requests.is_empty() => {
                    if completed.is_some_and(|result| result.is_err()) {
                        self.status.send_modify(|status| status.asr = SpeechServiceStatusSnapshot::Degraded);
                    }
                }
            }
        }
        requests.abort_all();
        while requests.join_next().await.is_some() {}
        Ok(())
    }
}

impl SpeechServiceHandle {
    pub(crate) fn status(&self) -> DeviceSpeechServicesSnapshot {
        *self.status.borrow()
    }

    pub(crate) fn asr_ready(&self) -> bool {
        self.status().asr == SpeechServiceStatusSnapshot::Ready
    }

    pub(crate) fn tts_ready(&self) -> bool {
        self.status().tts == SpeechServiceStatusSnapshot::Ready
    }

    pub(crate) fn subscribe_status(&self) -> watch::Receiver<DeviceSpeechServicesSnapshot> {
        self.status.clone()
    }

    pub(crate) async fn reload(&self) {
        let (response_tx, response_rx) = oneshot::channel();
        if self
            .commands
            .send(SpeechCommand::Reload {
                response: response_tx,
            })
            .await
            .is_ok()
        {
            let _ = response_rx.await;
        }
    }

    pub(crate) async fn recognize(
        &self,
        pcm: Vec<u8>,
        debug_name: String,
        cancellation: CancellationToken,
    ) -> Result<String, SpeechServiceError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.commands
            .send(SpeechCommand::Recognize {
                request: RecognizeRequest {
                    pcm: Arc::from(pcm),
                    debug_name,
                    cancellation,
                },
                response: response_tx,
            })
            .await
            .map_err(|_| SpeechServiceError::Unavailable)?;
        response_rx
            .await
            .map_err(|_| SpeechServiceError::Unavailable)?
    }

    pub(crate) async fn synthesize(
        &self,
        text: String,
        debug_name: String,
        cancellation: CancellationToken,
    ) -> Result<Arc<[u8]>, SpeechServiceError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.commands
            .send(SpeechCommand::Synthesize {
                request: SynthesizeRequest {
                    text,
                    debug_name,
                    cancellation,
                },
                response: response_tx,
            })
            .await
            .map_err(|_| SpeechServiceError::Unavailable)?;
        response_rx
            .await
            .map_err(|_| SpeechServiceError::Unavailable)?
    }
}

async fn recognize(
    provider: Option<Arc<dyn AsrProvider>>,
    debug_directory: Option<PathBuf>,
    request: RecognizeRequest,
    shutdown: CancellationToken,
) -> Result<String, SpeechServiceError> {
    let Some(provider) = provider else {
        return Err(SpeechServiceError::Unavailable);
    };
    if let Some(directory) = debug_directory {
        save_debug_audio(&directory, &request.debug_name, &request.pcm, "uplink").await;
    }
    let cancellation = request.cancellation;
    let provider_request = AsrRequest {
        pcm_s16le: request.pcm,
        cancellation: cancellation.clone(),
    };
    tokio::select! {
        () = shutdown.cancelled() => Err(SpeechServiceError::Cancelled),
        () = cancellation.cancelled() => Err(SpeechServiceError::Cancelled),
        result = provider.recognize(&provider_request) => result
            .map(|result| result.transcript)
            .map_err(SpeechServiceError::from),
    }
}

async fn synthesize(
    provider: Option<Arc<dyn TtsProvider>>,
    debug_directory: Option<PathBuf>,
    request: SynthesizeRequest,
    shutdown: CancellationToken,
) -> Result<Arc<[u8]>, SpeechServiceError> {
    let Some(provider) = provider else {
        return Err(SpeechServiceError::Unavailable);
    };
    let cancellation = request.cancellation;
    let result = tokio::select! {
        () = shutdown.cancelled() => return Err(SpeechServiceError::Cancelled),
        () = cancellation.cancelled() => return Err(SpeechServiceError::Cancelled),
        result = provider.synthesize(&request.text, cancellation.clone()) => result
            .map_err(SpeechServiceError::from)?,
    };
    if let Some(directory) = debug_directory {
        save_debug_audio(&directory, &request.debug_name, &result.pcm_s16le, "tts").await;
    }
    Ok(result.pcm_s16le)
}

async fn save_debug_audio(directory: &std::path::Path, name: &str, pcm: &[u8], suffix: &str) {
    if tokio::fs::create_dir_all(directory).await.is_err() {
        return;
    }
    let safe_name = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .take(160)
        .collect::<String>();
    let _ = tokio::fs::write(directory.join(format!("{safe_name}-{suffix}.pcm")), pcm).await;
}

/// SpeechService 向 Gateway 返回的实现无关错误分类。
///
/// Provider 的响应正文和凭据不会穿透该边界。
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum SpeechServiceError {
    #[error("speech service is unavailable")]
    Unavailable,
    #[error("speech request was cancelled")]
    Cancelled,
    #[error("speech provider authentication failed")]
    Authentication,
    #[error("speech provider request timed out")]
    Timeout,
    #[error("speech provider returned an invalid transcript")]
    InvalidTranscript,
    #[error("speech provider returned invalid audio")]
    InvalidAudio,
    #[error("speech provider audio exceeded the output limit")]
    OutputTooLarge,
    #[error("speech provider request failed")]
    ProviderFailed,
}

impl From<SpeechProviderError> for SpeechServiceError {
    fn from(error: SpeechProviderError) -> Self {
        match error {
            SpeechProviderError::Unavailable => Self::Unavailable,
            SpeechProviderError::Authentication => Self::Authentication,
            SpeechProviderError::Timeout => Self::Timeout,
            SpeechProviderError::InvalidTranscript | SpeechProviderError::InvalidPcm => {
                Self::InvalidTranscript
            }
            SpeechProviderError::InvalidAudio => Self::InvalidAudio,
            SpeechProviderError::OutputTooLarge => Self::OutputTooLarge,
            SpeechProviderError::Cancelled => Self::Cancelled,
            SpeechProviderError::ProviderFailed => Self::ProviderFailed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assistant_runtime::{
        ConfigDocument, ConfigSourceFuture, ConfigSourceLoad, RuntimeConfigSource,
    };
    use axum::{
        Json, Router,
        body::Bytes,
        routing::{get, post},
    };
    use serde_json::json;

    struct StaticSource(String);

    impl RuntimeConfigSource for StaticSource {
        fn load(&self) -> ConfigSourceFuture<'_> {
            Box::pin(std::future::ready(ConfigSourceLoad::Document(
                ConfigDocument::new(self.0.clone(), "test".to_owned()),
            )))
        }
    }

    #[tokio::test]
    async fn actor_owns_ready_provider_requests_and_shuts_them_down() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let app = Router::new().route(
            "/api/v1/services/aigc/multimodal-generation/generation",
            post(|| async { Json(json!({"output": {"text": "语音测试"}})) }),
        );
        let provider_server = tokio::spawn(async move { axum::serve(listener, app).await });
        let source = Arc::new(StaticSource(format!(
            r#"
[speech.asr]
provider = "dashscope"
model = "fixture-asr"
credential = "fixture-secret"
endpoint = "http://{address}"
timeout_ms = 2000
"#
        )));
        let (service, handle) = SpeechService::new(source);
        let mut status = handle.status.clone();
        let shutdown = CancellationToken::new();
        let service_task = tokio::spawn(service.run_until(shutdown.clone()));
        status.changed().await.expect("initial config status");
        assert!(handle.asr_ready());
        let transcript = handle
            .recognize(
                vec![0_u8; 640],
                "test-device-test-input".to_owned(),
                CancellationToken::new(),
            )
            .await
            .expect("transcript");
        assert_eq!(transcript, "语音测试");
        shutdown.cancel();
        service_task.await.expect("join").expect("shutdown");
        provider_server.abort();
        let _ = provider_server.await;
    }

    #[tokio::test]
    async fn actor_exposes_ready_tts_and_returns_pcm() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let audio_url = format!("http://{address}/speech.pcm");
        let app = Router::new()
            .route(
                "/api/v1/services/audio/tts/SpeechSynthesizer",
                post(move || {
                    let audio_url = audio_url.clone();
                    async move { Json(json!({"output": {"audio": {"url": audio_url}}})) }
                }),
            )
            .route(
                "/speech.pcm",
                get(|| async { Bytes::from_static(&[0_u8; 640]) }),
            );
        let provider_server = tokio::spawn(async move { axum::serve(listener, app).await });
        let source = Arc::new(StaticSource(format!(
            r#"
[speech.tts]
provider = "dashscope"
model = "fixture-tts"
voice = "fixture-voice"
credential = "fixture-secret"
endpoint = "http://{address}"
timeout_ms = 2000
"#
        )));
        let (service, handle) = SpeechService::new(source);
        let mut status = handle.status.clone();
        let shutdown = CancellationToken::new();
        let service_task = tokio::spawn(service.run_until(shutdown.clone()));
        status.changed().await.expect("initial config status");
        assert!(handle.tts_ready());
        let pcm = handle
            .synthesize(
                "简短播报".to_owned(),
                "test-output".to_owned(),
                CancellationToken::new(),
            )
            .await
            .expect("PCM");
        assert_eq!(pcm.len(), 640);
        shutdown.cancel();
        service_task.await.expect("join").expect("shutdown");
        provider_server.abort();
        let _ = provider_server.await;
    }
}
