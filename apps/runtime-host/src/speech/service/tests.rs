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
use std::{collections::VecDeque, sync::Mutex};

use super::super::provider::{AsrFuture, AsrResult, TtsFuture, TtsResult};

enum Step {
    Return(Result<(), SpeechProviderError>),
    Panic,
    Wait {
        started: oneshot::Sender<()>,
        result: oneshot::Receiver<Result<(), SpeechProviderError>>,
    },
}

struct ScriptedProvider(Mutex<VecDeque<Step>>);

impl ScriptedProvider {
    fn new(steps: impl IntoIterator<Item = Step>) -> Arc<Self> {
        Arc::new(Self(Mutex::new(steps.into_iter().collect())))
    }

    async fn execute(&self) -> Result<(), SpeechProviderError> {
        let step = self.0.lock().unwrap().pop_front().expect("script step");
        match step {
            Step::Return(result) => result,
            Step::Panic => panic!("injected provider panic"),
            Step::Wait { started, result } => {
                let _ = started.send(());
                result.await.expect("controlled completion")
            }
        }
    }
}

impl AsrProvider for ScriptedProvider {
    fn recognize<'a>(&'a self, _request: &'a AsrRequest) -> AsrFuture<'a> {
        Box::pin(async move {
            self.execute().await?;
            Ok(AsrResult {
                transcript: "有效输入".to_owned(),
            })
        })
    }
}

impl TtsProvider for ScriptedProvider {
    fn synthesize<'a>(&'a self, _text: &'a str, _cancellation: CancellationToken) -> TtsFuture<'a> {
        Box::pin(async move {
            self.execute().await?;
            Ok(TtsResult {
                pcm_s16le: Arc::from([0_u8; 640]),
            })
        })
    }
}

fn pending_step() -> (
    Step,
    oneshot::Receiver<()>,
    oneshot::Sender<Result<(), SpeechProviderError>>,
) {
    let (started_tx, started_rx) = oneshot::channel();
    let (result_tx, result_rx) = oneshot::channel();
    (
        Step::Wait {
            started: started_tx,
            result: result_rx,
        },
        started_rx,
        result_tx,
    )
}

struct RunningService {
    handle: SpeechServiceHandle,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<Result<(), SpeechServiceError>>,
}

impl RunningService {
    async fn start(asr: Arc<ScriptedProvider>, tts: Arc<ScriptedProvider>) -> Self {
        Self::with_config(asr, tts, "", Duration::from_secs(10)).await
    }

    async fn with_config(
        asr: Arc<ScriptedProvider>,
        tts: Arc<ScriptedProvider>,
        reload: &str,
        timeout: Duration,
    ) -> Self {
        let (service, handle) = SpeechService::new(Arc::new(StaticSource(reload.to_owned())));
        let mut status = handle.subscribe_status();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(service.serve_requests(
            config::CompiledSpeechConfig {
                asr: Some(asr),
                tts: Some(tts),
                status: DeviceSpeechServicesSnapshot {
                    asr: SpeechServiceStatusSnapshot::Ready,
                    tts: SpeechServiceStatusSnapshot::Ready,
                },
                debug_audio_directory: None,
                asr_timeout: timeout,
                tts_timeout: timeout,
            },
            shutdown.clone(),
        ));
        status.changed().await.unwrap();
        Self {
            handle,
            shutdown,
            task,
        }
    }

    async fn stop(self) {
        self.shutdown.cancel();
        self.task.await.unwrap().unwrap();
        assert_eq!(
            self.handle.status(),
            DeviceSpeechServicesSnapshot::default()
        );
        assert_eq!(self.handle.asr_slots.available_permits(), MAX_ASR_REQUESTS);
        assert_eq!(self.handle.tts_slots.available_permits(), MAX_TTS_REQUESTS);
    }
}

async fn asr(handle: &SpeechServiceHandle) -> Result<String, SpeechServiceError> {
    handle
        .recognize(vec![0; 640], "asr".to_owned(), CancellationToken::new())
        .await
}

async fn tts(handle: &SpeechServiceHandle) -> Result<Arc<[u8]>, SpeechServiceError> {
    handle
        .synthesize(
            "播报".to_owned(),
            "tts".to_owned(),
            CancellationToken::new(),
        )
        .await
}

#[tokio::test]
async fn provider_failures_and_panics_degrade_only_their_capability_and_next_success_recovers() {
    for is_asr in [true, false] {
        for failure in [
            Step::Return(Err(SpeechProviderError::Authentication)),
            Step::Return(Err(SpeechProviderError::Timeout)),
            Step::Return(Err(SpeechProviderError::ProviderFailed)),
            Step::Panic,
        ] {
            let failed = ScriptedProvider::new([failure, Step::Return(Ok(()))]);
            let healthy = ScriptedProvider::new([Step::Return(Ok(()))]);
            let service = if is_asr {
                RunningService::start(failed, healthy).await
            } else {
                RunningService::start(healthy, failed).await
            };
            if is_asr {
                assert!(asr(&service.handle).await.is_err());
                assert_eq!(
                    service.handle.status().asr,
                    SpeechServiceStatusSnapshot::Degraded
                );
                assert_eq!(
                    service.handle.status().tts,
                    SpeechServiceStatusSnapshot::Ready
                );
                assert!(service.handle.asr_available());
                assert!(tts(&service.handle).await.is_ok());
                assert!(asr(&service.handle).await.is_ok());
            } else {
                assert!(tts(&service.handle).await.is_err());
                assert_eq!(
                    service.handle.status().tts,
                    SpeechServiceStatusSnapshot::Degraded
                );
                assert_eq!(
                    service.handle.status().asr,
                    SpeechServiceStatusSnapshot::Ready
                );
                assert!(service.handle.tts_available());
                assert!(asr(&service.handle).await.is_ok());
                assert!(tts(&service.handle).await.is_ok());
            }
            assert_eq!(
                service.handle.status(),
                DeviceSpeechServicesSnapshot {
                    asr: SpeechServiceStatusSnapshot::Ready,
                    tts: SpeechServiceStatusSnapshot::Ready
                }
            );
            service.stop().await;
        }
    }
}

#[tokio::test]
async fn empty_transcript_is_not_a_provider_outage() {
    let service = RunningService::start(
        ScriptedProvider::new([Step::Return(Err(SpeechProviderError::InvalidTranscript))]),
        ScriptedProvider::new([]),
    )
    .await;
    assert_eq!(
        asr(&service.handle).await,
        Err(SpeechServiceError::InvalidTranscript)
    );
    assert_eq!(
        service.handle.status().asr,
        SpeechServiceStatusSnapshot::Ready
    );
    service.stop().await;
}

#[tokio::test]
async fn admission_is_bounded_per_capability_and_cancellation_releases_capacity() {
    let mut steps = Vec::new();
    let mut starts = Vec::new();
    let mut completions = Vec::new();
    for _ in 0..MAX_ASR_REQUESTS {
        let (step, started, completion) = pending_step();
        steps.push(step);
        starts.push(started);
        completions.push(completion);
    }
    steps.push(Step::Return(Ok(())));
    let service = RunningService::start(
        ScriptedProvider::new(steps),
        ScriptedProvider::new([Step::Return(Ok(()))]),
    )
    .await;
    let mut callers = Vec::new();
    let cancellation = CancellationToken::new();
    for started in starts {
        let handle = service.handle.clone();
        let token = cancellation.clone();
        callers.push(tokio::spawn(async move {
            handle
                .recognize(vec![0; 640], "bounded".to_owned(), token)
                .await
        }));
        started.await.unwrap();
    }
    assert_eq!(asr(&service.handle).await, Err(SpeechServiceError::Busy));
    assert_eq!(service.handle.asr_slots.available_permits(), 0);
    assert!(tts(&service.handle).await.is_ok());
    cancellation.cancel();
    for caller in callers {
        assert_eq!(caller.await.unwrap(), Err(SpeechServiceError::Cancelled));
    }
    // 调用方取消可早于 actor 回收完成；许可归还才是资源终态，不以 Future 返回推断。
    let permits = service
        .handle
        .asr_slots
        .clone()
        .acquire_many_owned(MAX_ASR_REQUESTS as u32)
        .await
        .unwrap();
    drop(permits);
    assert_eq!(
        service.handle.status().asr,
        SpeechServiceStatusSnapshot::Ready
    );
    assert!(asr(&service.handle).await.is_ok());
    assert!(completions.iter().all(oneshot::Sender::is_closed));
    service.stop().await;
}

#[tokio::test]
async fn tts_capacity_is_independent_and_shutdown_settles_all_requests() {
    let mut steps = Vec::new();
    let mut starts = Vec::new();
    let mut completions = Vec::new();
    for _ in 0..MAX_TTS_REQUESTS {
        let (step, started, completion) = pending_step();
        steps.push(step);
        starts.push(started);
        completions.push(completion);
    }
    let service = RunningService::start(
        ScriptedProvider::new([Step::Return(Ok(()))]),
        ScriptedProvider::new(steps),
    )
    .await;
    let mut callers = Vec::new();
    for started in starts {
        let handle = service.handle.clone();
        callers.push(tokio::spawn(async move { tts(&handle).await }));
        started.await.unwrap();
    }
    assert_eq!(tts(&service.handle).await, Err(SpeechServiceError::Busy));
    assert!(asr(&service.handle).await.is_ok());
    service.shutdown.cancel();
    for caller in callers {
        assert_eq!(caller.await.unwrap(), Err(SpeechServiceError::Cancelled));
    }
    assert!(completions.iter().all(oneshot::Sender::is_closed));
    service.stop().await;
}

#[tokio::test]
async fn dropped_caller_cancels_provider_and_releases_permit() {
    let (step, started, mut completion) = pending_step();
    let service =
        RunningService::start(ScriptedProvider::new([step]), ScriptedProvider::new([])).await;
    let handle = service.handle.clone();
    let caller = tokio::spawn(async move { asr(&handle).await });
    started.await.unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    completion.closed().await;
    // 一次 actor round-trip 确保完成消息已回收，不用 sleep 猜测调度时序。
    service.handle.reload().await;
    assert_eq!(
        service.handle.asr_slots.available_permits(),
        MAX_ASR_REQUESTS
    );
    service.stop().await;
}

#[tokio::test]
async fn request_deadline_covers_an_unresponsive_provider() {
    let (step, _started, mut completion) = pending_step();
    let service = RunningService::with_config(
        ScriptedProvider::new([step]),
        ScriptedProvider::new([]),
        "",
        Duration::from_millis(5),
    )
    .await;
    assert_eq!(asr(&service.handle).await, Err(SpeechServiceError::Timeout));
    completion.closed().await;
    assert_eq!(
        service.handle.status().asr,
        SpeechServiceStatusSnapshot::Degraded
    );
    service.stop().await;
}

#[tokio::test]
async fn reload_rejects_old_provider_health_but_still_delivers_its_result() {
    for result in [Ok(()), Err(SpeechProviderError::Authentication)] {
        let (step, started, completion) = pending_step();
        let service = RunningService::with_config(
            ScriptedProvider::new([step]),
            ScriptedProvider::new([]),
            r#"
[speech.asr]
provider = "dashscope"
model = "replacement"
credential = "fixture"
endpoint = "http://127.0.0.1:9"
"#,
            Duration::from_secs(10),
        )
        .await;
        let handle = service.handle.clone();
        let caller = tokio::spawn(async move { asr(&handle).await });
        started.await.unwrap();
        service.handle.reload().await;
        completion.send(result).unwrap();
        assert_eq!(caller.await.unwrap().is_ok(), result.is_ok());
        assert_eq!(
            service.handle.status().asr,
            SpeechServiceStatusSnapshot::Ready
        );
        assert_eq!(
            service.handle.status().tts,
            SpeechServiceStatusSnapshot::Unavailable
        );
        service.stop().await;
    }
}

#[tokio::test]
async fn removed_provider_cannot_be_revived_by_late_success() {
    let (step, started, completion) = pending_step();
    let service =
        RunningService::start(ScriptedProvider::new([]), ScriptedProvider::new([step])).await;
    let handle = service.handle.clone();
    let caller = tokio::spawn(async move { tts(&handle).await });
    started.await.unwrap();
    service.handle.reload().await;
    completion.send(Ok(())).unwrap();
    assert!(caller.await.unwrap().is_ok());
    assert_eq!(
        service.handle.status(),
        DeviceSpeechServicesSnapshot::default()
    );
    service.stop().await;
}

#[tokio::test]
async fn service_abort_invalidates_snapshot_and_releases_tasks() {
    let (step, started, mut completion) = pending_step();
    let service =
        RunningService::start(ScriptedProvider::new([step]), ScriptedProvider::new([])).await;
    let handle = service.handle.clone();
    let caller = tokio::spawn(async move { asr(&handle).await });
    started.await.unwrap();
    service.task.abort();
    assert!(service.task.await.unwrap_err().is_cancelled());
    assert_eq!(
        service.handle.status(),
        DeviceSpeechServicesSnapshot::default()
    );
    assert_eq!(caller.await.unwrap(), Err(SpeechServiceError::Unavailable));
    completion.closed().await;
    assert!(!service.handle.asr_available());
    assert!(!service.handle.tts_available());
    assert_eq!(
        service.handle.asr_slots.available_permits(),
        MAX_ASR_REQUESTS
    );
}

#[tokio::test]
async fn invalid_and_precancelled_requests_do_not_reach_provider_or_consume_capacity() {
    let service = RunningService::start(ScriptedProvider::new([]), ScriptedProvider::new([])).await;
    for pcm in [vec![], vec![0], vec![0; MAX_ASR_PCM_BYTES + 2]] {
        assert_eq!(
            service
                .handle
                .recognize(pcm, "invalid".to_owned(), CancellationToken::new())
                .await,
            Err(SpeechServiceError::InvalidInput)
        );
    }
    assert_eq!(
        service
            .handle
            .synthesize(
                "字".repeat(MAX_TTS_TEXT_CHARS + 1),
                "invalid".to_owned(),
                CancellationToken::new()
            )
            .await,
        Err(SpeechServiceError::InvalidInput)
    );
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        service
            .handle
            .recognize(vec![0; 640], "cancelled".to_owned(), cancellation.clone())
            .await,
        Err(SpeechServiceError::Cancelled)
    );
    assert_eq!(
        service
            .handle
            .synthesize("播报".to_owned(), "cancelled".to_owned(), cancellation)
            .await,
        Err(SpeechServiceError::Cancelled)
    );
    service.stop().await;
}

struct StaticSource(String);

#[tokio::test]
async fn shutdown_cancels_queued_media_before_provider_dispatch() {
    let (service, handle) = SpeechService::new(Arc::new(StaticSource(String::new())));
    let (asr_tx, asr_rx) = oneshot::channel();
    let (tts_tx, tts_rx) = oneshot::channel();
    handle
        .commands
        .try_send(SpeechCommand::Recognize {
            request: RecognizeRequest {
                queued_at: std::time::Instant::now(),
                pcm: Arc::from([0_u8; 640]),
                debug_name: "queued".to_owned(),
                cancellation: CancellationToken::new(),
            },
            response: asr_tx,
            permit: handle.asr_slots.clone().try_acquire_owned().unwrap(),
        })
        .unwrap_or_else(|_| panic!("enqueue ASR"));
    handle
        .commands
        .try_send(SpeechCommand::Synthesize {
            request: SynthesizeRequest {
                queued_at: std::time::Instant::now(),
                text: "播报".to_owned(),
                debug_name: "queued".to_owned(),
                cancellation: CancellationToken::new(),
            },
            response: tts_tx,
            permit: handle.tts_slots.clone().try_acquire_owned().unwrap(),
        })
        .unwrap_or_else(|_| panic!("enqueue TTS"));
    let shutdown = CancellationToken::new();
    shutdown.cancel();
    service
        .serve_requests(config::CompiledSpeechConfig::unavailable(), shutdown)
        .await
        .unwrap();
    assert_eq!(asr_rx.await.unwrap(), Err(SpeechServiceError::Cancelled));
    assert_eq!(tts_rx.await.unwrap(), Err(SpeechServiceError::Cancelled));
    assert_eq!(handle.asr_slots.available_permits(), MAX_ASR_REQUESTS);
    assert_eq!(handle.tts_slots.available_permits(), MAX_TTS_REQUESTS);
    assert_eq!(handle.status(), DeviceSpeechServicesSnapshot::default());
}

#[tokio::test]
async fn shutdown_does_not_wait_for_initial_configuration_io() {
    struct PendingSource;
    impl RuntimeConfigSource for PendingSource {
        fn load(&self) -> ConfigSourceFuture<'_> {
            Box::pin(std::future::pending())
        }
    }
    let (service, handle) = SpeechService::new(Arc::new(PendingSource));
    let shutdown = CancellationToken::new();
    let mut running = Box::pin(service.run_until(shutdown.clone()));
    assert!(futures_util::poll!(running.as_mut()).is_pending());
    shutdown.cancel();
    running.await.unwrap();
    assert_eq!(handle.status(), DeviceSpeechServicesSnapshot::default());
}

#[tokio::test]
async fn caller_cancellation_does_not_wait_for_a_queued_command_to_be_dispatched() {
    let (service, handle) = SpeechService::new(Arc::new(StaticSource(String::new())));
    // 暂不调度 actor，确定性模拟配置 I/O 期间命令已入队但尚未 dispatch 的窗口。
    service.status.send_replace(DeviceSpeechServicesSnapshot {
        asr: SpeechServiceStatusSnapshot::Ready,
        tts: SpeechServiceStatusSnapshot::Ready,
    });
    let token = CancellationToken::new();
    let mut recognition =
        Box::pin(handle.recognize(vec![0; 640], "queued".to_owned(), token.clone()));
    let mut synthesis =
        Box::pin(handle.synthesize("播报".to_owned(), "queued".to_owned(), token.clone()));
    assert!(futures_util::poll!(recognition.as_mut()).is_pending());
    assert!(futures_util::poll!(synthesis.as_mut()).is_pending());
    token.cancel();
    assert_eq!(recognition.await, Err(SpeechServiceError::Cancelled));
    assert_eq!(synthesis.await, Err(SpeechServiceError::Cancelled));
    drop(service);
    assert_eq!(handle.asr_slots.available_permits(), MAX_ASR_REQUESTS);
    assert_eq!(handle.tts_slots.available_permits(), MAX_TTS_REQUESTS);
}

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
    assert!(handle.asr_available());
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
    assert!(handle.tts_available());
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
