//! SpeechService actor：原子交换 Provider 配置并拥有全部并发语音请求。

use std::{future::Future, panic::AssertUnwindSafe, path::PathBuf, sync::Arc, time::Duration};

use crate::media_diagnostics::{correlation_id, timestamp_ms};
use assistant_protocol::{DeviceSpeechServicesSnapshot, SpeechServiceStatusSnapshot};
use assistant_runtime::RuntimeConfigSource;
use futures_util::FutureExt;
use thiserror::Error;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use super::{
    config,
    provider::{AsrProvider, AsrRequest, SpeechProviderError, TtsProvider},
};

const COMMAND_CAPACITY: usize = 32;
// 准入许可贯穿命令排队、Provider 执行和结果回收，满载直接拒绝，不创建持有 PCM 的等待队列。
const MAX_ASR_REQUESTS: usize = 4;
const MAX_TTS_REQUESTS: usize = 2;
const MAX_ASR_PCM_BYTES: usize = 60 * 16_000 * 2;
const MAX_TTS_TEXT_CHARS: usize = 120;

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
    asr_slots: Arc<Semaphore>,
    tts_slots: Arc<Semaphore>,
}

/// 一次整句 ASR 请求及其独立取消边界。
struct RecognizeRequest {
    queued_at: std::time::Instant,
    pcm: Arc<[u8]>,
    debug_name: String,
    cancellation: CancellationToken,
}

/// 一次短文本 TTS 请求及其独立取消边界。
struct SynthesizeRequest {
    queued_at: std::time::Instant,
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
        permit: OwnedSemaphorePermit,
    },
    Synthesize {
        request: SynthesizeRequest,
        response: oneshot::Sender<Result<Arc<[u8]>, SpeechServiceError>>,
        permit: OwnedSemaphorePermit,
    },
}

/// 请求完成仍由 actor 回收：先更新当前 Provider 健康，再回复调用方并释放准入许可。
/// Provider Arc 身份用于隔离 reload 前的迟到结果，不另建持久 generation 或业务状态。
struct RequestCompletion<T, P: ?Sized> {
    provider: Option<Arc<P>>,
    result: Result<T, SpeechServiceError>,
    response: oneshot::Sender<Result<T, SpeechServiceError>>,
    _permit: OwnedSemaphorePermit,
}

impl<T, P: ?Sized> RequestCompletion<T, P> {
    fn finish(
        self,
        current: &Option<Arc<P>>,
        projection: &watch::Sender<DeviceSpeechServicesSnapshot>,
        capability: fn(&mut DeviceSpeechServicesSnapshot) -> &mut SpeechServiceStatusSnapshot,
    ) {
        if self
            .provider
            .as_ref()
            .zip(current.as_ref())
            .is_some_and(|(old, new)| Arc::ptr_eq(old, new))
        {
            let observed = match &self.result {
                // 空识别也是一次有效 Provider 响应；不把用户静默视为服务故障。
                Ok(_) | Err(SpeechServiceError::InvalidTranscript) => {
                    Some(SpeechServiceStatusSnapshot::Ready)
                }
                Err(SpeechServiceError::Cancelled) => None,
                Err(_) => Some(SpeechServiceStatusSnapshot::Degraded),
            };
            if let Some(observed) = observed {
                let changed = projection.send_if_modified(|snapshot| {
                    let status = capability(snapshot);
                    let changed = *status != observed;
                    *status = observed;
                    changed
                });
                if changed {
                    let snapshot = *projection.borrow();
                    eprintln!(
                        "event=speech_service_health ts_ms={} asr={:?} tts={:?}",
                        timestamp_ms(),
                        snapshot.asr,
                        snapshot.tts
                    );
                }
            }
        }
        // watch 写锁已释放；调用方收到终态后读取的一定是已更新的健康投影。
        let _ = self.response.send(self.result);
    }
}

impl Drop for SpeechService {
    fn drop(&mut self) {
        // 包括 supervisor abort/panic；watch 的最后一份值不能永久残留为 ready。
        self.status
            .send_replace(DeviceSpeechServicesSnapshot::default());
    }
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
                asr_slots: Arc::new(Semaphore::new(MAX_ASR_REQUESTS)),
                tts_slots: Arc::new(Semaphore::new(MAX_TTS_REQUESTS)),
            },
        )
    }

    pub(crate) async fn run_until(
        self,
        shutdown: CancellationToken,
    ) -> Result<(), SpeechServiceError> {
        let compiled = tokio::select! {
            biased;
            () = shutdown.cancelled() => return Ok(()),
            compiled = config::load(self.source.as_ref()) => compiled,
        };
        self.serve_requests(compiled, shutdown).await
    }

    /// 配置只在 actor 边界交换；在途请求保留原 Provider，完成投影按 Arc 身份核对。
    async fn serve_requests(
        mut self,
        mut compiled: config::CompiledSpeechConfig,
        shutdown: CancellationToken,
    ) -> Result<(), SpeechServiceError> {
        let shutdown = shutdown.child_token();
        let _shutdown_guard = shutdown.clone().drop_guard();
        self.status.send_replace(compiled.status);
        let mut recognition = JoinSet::<RequestCompletion<String, dyn AsrProvider>>::new();
        let mut synthesis = JoinSet::<RequestCompletion<Arc<[u8]>, dyn TtsProvider>>::new();
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => break,
                completed = recognition.join_next(), if !recognition.is_empty() => {
                    match completed {
                        Some(Ok(completed)) => completed.finish(&compiled.asr, &self.status, |status| &mut status.asr),
                        Some(Err(_)) => self.status.send_modify(|status| status.asr = SpeechServiceStatusSnapshot::Degraded),
                        None => {},
                    }
                }
                completed = synthesis.join_next(), if !synthesis.is_empty() => {
                    match completed {
                        Some(Ok(completed)) => completed.finish(&compiled.tts, &self.status, |status| &mut status.tts),
                        Some(Err(_)) => self.status.send_modify(|status| status.tts = SpeechServiceStatusSnapshot::Degraded),
                        None => {},
                    }
                }
                command = self.commands.recv() => {
                    let Some(command) = command else { break };
                    match command {
                        SpeechCommand::Reload { response } => {
                            compiled = tokio::select! {
                                biased;
                                () = shutdown.cancelled() => break,
                                compiled = config::load(self.source.as_ref()) => compiled,
                            };
                            self.status.send_replace(compiled.status);
                            let _ = response.send(());
                        }
                        SpeechCommand::Recognize { request, mut response, permit } => {
                            let request_id = correlation_id(&request.debug_name);
                            let started_at = std::time::Instant::now();
                            eprintln!("event=speech_request_started ts_ms={} request={} capability=asr queue_ms={} input_bytes={} in_flight={} command_depth={}", timestamp_ms(), request_id, request.queued_at.elapsed().as_millis(), request.pcm.len(), recognition.len() + 1, self.commands.len());
                            let provider = compiled.asr.clone();
                            let timeout = compiled.asr_timeout;
                            let debug_directory = compiled.debug_audio_directory.clone();
                            let request_shutdown = shutdown.child_token();
                            recognition.spawn(async move {
                                let cancellation = request.cancellation.clone();
                                let result = run_request(recognize(provider.clone(), debug_directory, request, request_shutdown.clone()), &mut response, cancellation, request_shutdown, timeout).await;
                                eprintln!("event=speech_request_finished ts_ms={} request={} capability=asr elapsed_ms={} result={}", timestamp_ms(), request_id, started_at.elapsed().as_millis(), result.as_ref().err().map_or("ok", SpeechServiceError::code));
                                RequestCompletion { provider, result, response, _permit: permit }
                            });
                        }
                        SpeechCommand::Synthesize { request, mut response, permit } => {
                            let request_id = correlation_id(&request.debug_name);
                            let started_at = std::time::Instant::now();
                            eprintln!("event=speech_request_started ts_ms={} request={} capability=tts queue_ms={} input_bytes={} in_flight={} command_depth={}", timestamp_ms(), request_id, request.queued_at.elapsed().as_millis(), request.text.len(), synthesis.len() + 1, self.commands.len());
                            let provider = compiled.tts.clone();
                            let timeout = compiled.tts_timeout;
                            let debug_directory = compiled.debug_audio_directory.clone();
                            let request_shutdown = shutdown.child_token();
                            synthesis.spawn(async move {
                                let cancellation = request.cancellation.clone();
                                let result = run_request(synthesize(provider.clone(), debug_directory, request, request_shutdown.clone()), &mut response, cancellation, request_shutdown, timeout).await;
                                eprintln!("event=speech_request_finished ts_ms={} request={} capability=tts elapsed_ms={} pcm_bytes={} result={}", timestamp_ms(), request_id, started_at.elapsed().as_millis(), result.as_ref().map_or(0, |pcm| pcm.len()), result.as_ref().err().map_or("ok", SpeechServiceError::code));
                                RequestCompletion { provider, result, response, _permit: permit }
                            });
                        }
                    }
                }
            }
        }
        self.commands.close();
        self.status
            .send_replace(DeviceSpeechServicesSnapshot::default());
        shutdown.cancel();
        while let Ok(command) = self.commands.try_recv() {
            match command {
                SpeechCommand::Reload { response } => {
                    let _ = response.send(());
                }
                SpeechCommand::Recognize { response, .. } => {
                    let _ = response.send(Err(SpeechServiceError::Cancelled));
                }
                SpeechCommand::Synthesize { response, .. } => {
                    let _ = response.send(Err(SpeechServiceError::Cancelled));
                }
            }
        }
        // run_request 包围完整 Provider/debug I/O；取消后正常收口，不直接 abort 丢失终态。
        while let Some(completed) = recognition.join_next().await {
            if let Ok(completed) = completed {
                completed.finish(&None, &self.status, |status| &mut status.asr);
            }
        }
        while let Some(completed) = synthesis.join_next().await {
            if let Ok(completed) = completed {
                completed.finish(&None, &self.status, |status| &mut status.tts);
            }
        }
        Ok(())
    }
}

impl SpeechServiceHandle {
    pub(crate) fn status(&self) -> DeviceSpeechServicesSnapshot {
        if self.commands.is_closed() {
            DeviceSpeechServicesSnapshot::default()
        } else {
            *self.status.borrow()
        }
    }

    pub(crate) fn asr_available(&self) -> bool {
        // degraded 仍接受下一次显式请求来验证恢复，禁止把降级变成无法自愈的准入死锁。
        self.status().asr != SpeechServiceStatusSnapshot::Unavailable
    }

    pub(crate) fn tts_available(&self) -> bool {
        self.status().tts != SpeechServiceStatusSnapshot::Unavailable
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
        if cancellation.is_cancelled() {
            return Err(SpeechServiceError::Cancelled);
        }
        if !self.asr_available() {
            return Err(SpeechServiceError::Unavailable);
        }
        if pcm.is_empty() || !pcm.len().is_multiple_of(2) || pcm.len() > MAX_ASR_PCM_BYTES {
            return Err(SpeechServiceError::InvalidInput);
        }
        let permit =
            self.asr_slots.clone().try_acquire_owned().map_err(|_| {
                log_admission_rejection("asr", &debug_name, SpeechServiceError::Busy)
            })?;
        let (response_tx, response_rx) = oneshot::channel();
        self.commands
            .try_send(SpeechCommand::Recognize {
                request: RecognizeRequest {
                    queued_at: std::time::Instant::now(),
                    pcm: Arc::from(pcm),
                    debug_name,
                    cancellation: cancellation.clone(),
                },
                response: response_tx,
                permit,
            })
            .map_err(|_| SpeechServiceError::Unavailable)?;
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(SpeechServiceError::Cancelled),
            response = response_rx => response.map_err(|_| SpeechServiceError::Unavailable)?,
        }
    }

    pub(crate) async fn synthesize(
        &self,
        text: String,
        debug_name: String,
        cancellation: CancellationToken,
    ) -> Result<Arc<[u8]>, SpeechServiceError> {
        if cancellation.is_cancelled() {
            return Err(SpeechServiceError::Cancelled);
        }
        if !self.tts_available() {
            return Err(SpeechServiceError::Unavailable);
        }
        if text.trim().is_empty() || text.chars().count() > MAX_TTS_TEXT_CHARS {
            return Err(SpeechServiceError::InvalidInput);
        }
        let permit =
            self.tts_slots.clone().try_acquire_owned().map_err(|_| {
                log_admission_rejection("tts", &debug_name, SpeechServiceError::Busy)
            })?;
        let (response_tx, response_rx) = oneshot::channel();
        self.commands
            .try_send(SpeechCommand::Synthesize {
                request: SynthesizeRequest {
                    queued_at: std::time::Instant::now(),
                    text,
                    debug_name,
                    cancellation: cancellation.clone(),
                },
                response: response_tx,
                permit,
            })
            .map_err(|_| SpeechServiceError::Unavailable)?;
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(SpeechServiceError::Cancelled),
            response = response_rx => response.map_err(|_| SpeechServiceError::Unavailable)?,
        }
    }
}

/// 父服务、调用方取消和接收者消失均结束整次请求；panic 只映射稳定错误，不传播 payload。
async fn run_request<T>(
    request: impl Future<Output = Result<T, SpeechServiceError>>,
    response: &mut oneshot::Sender<Result<T, SpeechServiceError>>,
    cancellation: CancellationToken,
    shutdown: CancellationToken,
    timeout: Duration,
) -> Result<T, SpeechServiceError> {
    tokio::select! {
        biased;
        () = shutdown.cancelled() => Err(SpeechServiceError::Cancelled),
        () = cancellation.cancelled() => Err(SpeechServiceError::Cancelled),
        () = response.closed() => Err(SpeechServiceError::Cancelled),
        () = tokio::time::sleep(timeout) => Err(SpeechServiceError::Timeout),
        result = AssertUnwindSafe(request).catch_unwind() => result.unwrap_or(Err(SpeechServiceError::ProviderFailed)),
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
    #[error("speech service request capacity is exhausted")]
    Busy,
    #[error("speech request input is invalid or exceeds its limit")]
    InvalidInput,
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

impl SpeechServiceError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::InvalidInput => "invalid_input",
            Self::Unavailable => "unavailable",
            Self::Cancelled => "cancelled",
            Self::Authentication => "authentication",
            Self::Timeout => "timeout",
            Self::InvalidTranscript => "invalid_transcript",
            Self::InvalidAudio => "invalid_audio",
            Self::OutputTooLarge => "output_too_large",
            Self::ProviderFailed => "provider_failed",
        }
    }
}

fn log_admission_rejection(
    capability: &'static str,
    request: &str,
    error: SpeechServiceError,
) -> SpeechServiceError {
    eprintln!(
        "event=speech_request_rejected ts_ms={} request={} capability={} result={}",
        timestamp_ms(),
        correlation_id(request),
        capability,
        error.code()
    );
    error
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
mod tests;
