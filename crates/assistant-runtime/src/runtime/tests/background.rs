use std::path::Path;

use agent_tools::{
    AbsolutePath, BackgroundOutputRequest, BackgroundProcess, BackgroundShell,
    BackgroundShellFuture, BackgroundShellRequest, ShellToolError,
};
use agent_tools_local::{EnvironmentPolicy, LocalBackgroundShell, LocalShell, LocalShellConfig};
use assistant_protocol::{PrepareDeleteSessionRequest, RunStatus};

use super::*;
use crate::{
    BackgroundRunOrigin, BackgroundShellStart, RunToolFactoryRequest, StartBackgroundShell,
};

const BUDGET: Duration = Duration::from_secs(5);
#[cfg(unix)]
const COMMAND: &str = "printf ready; exec sleep 30";
#[cfg(windows)]
const COMMAND: &str = "<nul set /p =ready & ping -n 31 127.0.0.1 >NUL";

struct Factory(Arc<dyn BackgroundShell>);

impl RunToolFactory for Factory {
    fn compile(&self, _: RunToolFactoryRequest<'_>) -> Result<RunToolBundle, RunToolFactoryError> {
        Ok(RunToolBundle::new(ToolSetSnapshot::default(), Vec::new()))
    }
    fn background_shell(
        &self,
        _: Option<&crate::FrozenShellEnvironment>,
    ) -> Option<Arc<dyn BackgroundShell>> {
        Some(self.0.clone())
    }
}

struct PausedHandoff {
    shell: LocalBackgroundShell,
    ready: Notify,
    release: Notify,
    path: Mutex<Option<std::path::PathBuf>>,
}

impl BackgroundShell for PausedHandoff {
    fn start<'a>(
        &'a self,
        id: &'a str,
        request: BackgroundShellRequest,
    ) -> BackgroundShellFuture<'a, Box<dyn BackgroundProcess>> {
        Box::pin(async move {
            let process = self.shell.start(id, request).await?;
            *self.path.lock().unwrap() = Some(process.output().path().to_owned());
            self.ready.notify_one();
            self.release.notified().await;
            Ok(process)
        })
    }
}

fn shell(home: &Path) -> LocalBackgroundShell {
    LocalBackgroundShell::new(
        Arc::new(LocalShell::new(LocalShellConfig::new(
            EnvironmentPolicy::default(),
        ))),
        home.to_owned(),
    )
}

async fn active_runtime(
    adapter: Arc<dyn BackgroundShell>,
) -> (AssistantRuntime, BackgroundRunOrigin, CancellationToken) {
    let entered = Arc::new(Notify::new());
    let runtime = runtime_with_run_tool_factory(
        Arc::new(CancellationAwareModel {
            capabilities: model_capabilities(false),
            entered: entered.clone(),
        }),
        Arc::new(Factory(adapter)),
    );
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let run = runtime
        .submit_started_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "background lifecycle fixture".into(),
            variant: AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            delivery: assistant_protocol::InputDelivery::Queued,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .unwrap()
        .run;
    tokio::time::timeout(BUDGET, entered.notified())
        .await
        .unwrap();
    let cancellation = runtime
        .session_for_test(&session_id)
        .await
        .lock_state()
        .unwrap()
        .active_run
        .as_ref()
        .unwrap()
        .cancellation
        .clone();
    (
        runtime,
        BackgroundRunOrigin {
            session_id,
            run_id: run.run_id,
            call_id: ToolCallId::new("test-background").unwrap(),
        },
        cancellation,
    )
}

fn request(home: &Path) -> StartBackgroundShell {
    StartBackgroundShell {
        shell: BackgroundShellRequest {
            command: COMMAND.into(),
            workdir: AbsolutePath::new(home.to_owned()).unwrap(),
        },
        wait: Some(Duration::ZERO),
        timeout: None,
    }
}

#[tokio::test]
async fn handed_off_shell_survives_run_and_is_reaped_by_runtime_shutdown() {
    let home = tempfile::tempdir().unwrap();
    let (runtime, origin, cancellation) = active_runtime(Arc::new(shell(home.path()))).await;
    let first = runtime
        .start_background_shell(origin.clone(), request(home.path()), cancellation.clone())
        .await
        .unwrap();
    let BackgroundShellStart::Running { id, log_path, .. } = first else {
        panic!("expected running")
    };
    let repeated = runtime
        .start_background_shell(origin.clone(), request(home.path()), cancellation)
        .await
        .unwrap();
    assert!(
        matches!(repeated, BackgroundShellStart::Running { id: repeated, .. } if repeated == id)
    );

    runtime
        .cancel_run(CancelRunRequest {
            session_id: origin.session_id.clone(),
            run_id: origin.run_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &origin.session_id, &origin.run_id)
            .await
            .status,
        RunStatus::Cancelled
    );
    assert_eq!(
        runtime.background_shell_ids(&origin.session_id),
        std::slice::from_ref(&id)
    );
    assert!(runtime.has_background_work());
    assert!(!runtime.request_shutdown_if_unused(|| true).await.unwrap());
    assert!(
        runtime
            .prepare_delete_session(PrepareDeleteSessionRequest {
                session_id: origin.session_id.clone(),
            })
            .await
            .is_err()
    );

    let other = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    assert!(runtime.stop_background_shell(&other, &id).await.is_err());
    let read = BackgroundOutputRequest {
        cursor: None,
        offset_chars: 0,
        limit_chars: 10,
    };
    assert!(
        runtime
            .read_background_shell(&other, &id, read)
            .await
            .is_err()
    );
    tokio::time::timeout(BUDGET, async {
        loop {
            let latest = runtime
                .read_background_shell(&origin.session_id, &id, read)
                .await
                .unwrap();
            if latest.content.contains("ready") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    assert_eq!(runtime.preview_shutdown().await.unwrap().counts().0, 1);
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
    assert!(!runtime.has_background_work());
    assert!(runtime.background_shell_ids(&origin.session_id).is_empty());
    assert!(!log_path.exists());
}

#[tokio::test]
async fn cancellation_before_handoff_cleans_the_already_started_process() {
    let home = tempfile::tempdir().unwrap();
    let adapter = Arc::new(PausedHandoff {
        shell: shell(home.path()),
        ready: Notify::new(),
        release: Notify::new(),
        path: Mutex::new(None),
    });
    let (runtime, origin, cancellation) = active_runtime(adapter.clone()).await;
    let runtime = Arc::new(runtime);
    let launch = {
        let runtime = runtime.clone();
        let origin = origin.clone();
        let cancellation = cancellation.clone();
        let request = request(home.path());
        tokio::spawn(async move {
            runtime
                .start_background_shell(origin, request, cancellation)
                .await
        })
    };
    tokio::time::timeout(BUDGET, adapter.ready.notified())
        .await
        .unwrap();
    let log = adapter.path.lock().unwrap().clone().unwrap();
    assert!(log.exists());
    cancellation.cancel();
    adapter.release.notify_one();
    assert_eq!(
        tokio::time::timeout(BUDGET, launch).await.unwrap().unwrap(),
        Err(ShellToolError::Cancelled)
    );
    assert!(runtime.background_shell_ids(&origin.session_id).is_empty());
    assert!(!log.exists());
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn external_exit_removes_registration_without_querying_or_stopping() {
    let home = tempfile::tempdir().unwrap();
    let (runtime, origin, cancellation) = active_runtime(Arc::new(shell(home.path()))).await;
    let result = runtime
        .start_background_shell(origin.clone(), request(home.path()), cancellation)
        .await
        .unwrap();
    let BackgroundShellStart::Running { id, log_path, .. } = result else {
        panic!("expected running")
    };
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(log_path.with_file_name("identity.json")).unwrap())
            .unwrap();
    let pid = record["pid"].as_u64().unwrap();
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let mut changed = runtime.subscribe_work_changes();
    // 订阅通知观察自动收尾，过程中不调用列表、读取或停止入口。
    tokio::time::timeout(BUDGET, async {
        while log_path.exists() {
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(runtime.background_shell_ids(&origin.session_id).is_empty());
    assert!(
        runtime
            .stop_background_shell(&origin.session_id, &id)
            .await
            .is_err()
    );
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn concurrent_stops_receive_one_settlement() {
    let home = tempfile::tempdir().unwrap();
    let (runtime, origin, cancellation) = active_runtime(Arc::new(shell(home.path()))).await;
    let result = runtime
        .start_background_shell(origin.clone(), request(home.path()), cancellation)
        .await
        .unwrap();
    let BackgroundShellStart::Running { id, log_path, .. } = result else {
        panic!("expected running");
    };
    let (first, second) = tokio::time::timeout(BUDGET, async {
        tokio::join!(
            runtime.stop_background_shell(&origin.session_id, &id),
            runtime.stop_background_shell(&origin.session_id, &id),
        )
    })
    .await
    .unwrap();
    let exit = first.unwrap();
    assert_eq!(second.unwrap(), exit);
    assert!(!exit.timed_out);
    assert!(!log_path.exists());
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
}
