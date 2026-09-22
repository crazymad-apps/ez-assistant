//! 认证与监听先于业务初始化；失败保留诊断监听，业务服务和监督任务只在真实初始化成功后发布。

use crate::{
    access,
    config::ServeConfig,
    config_source::{LocalConfigSource, prepare_runtime_home},
    endpoint::RuntimeInstanceGuard,
    http::{HttpEndpointState, HttpState},
    server::RuntimeServer,
    supervisor::{FailurePolicy, HostSupervisor},
    user_terminal,
};
use std::{error::Error, sync::Arc, time::Duration};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) async fn serve(config: ServeConfig) -> Result<(), Box<dyn Error>> {
    #[cfg(unix)]
    let (mut terminate, mut interrupt) = {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        (terminate, interrupt)
    };
    // Windows 无 SIGTERM/SIGINT 流；两个 Ctrl+C 流独立接收同一控制台事件。
    #[cfg(windows)]
    let mut interrupt = tokio::signal::windows::ctrl_c()?;
    #[cfg(windows)]
    let mut terminate = tokio::signal::windows::ctrl_c()?;
    prepare_runtime_home(&config.runtime_home)?;
    let instance = RuntimeInstanceGuard::acquire(&config.runtime_home)?;
    let layout_home = config.runtime_home.clone();
    tokio::task::spawn_blocking(move || crate::host_layout::upgrade(&layout_home)).await??;
    let host_source = Arc::new(LocalConfigSource::new(config.config_path.clone()));
    let (mut access_service, access_handle) = access::HostAccessService::new(host_source);
    if config.password_stdin {
        use tokio::io::AsyncReadExt as _;
        let mut bytes = Vec::new();
        let mut input = tokio::io::stdin().take(1027);
        tokio::select! {
            result = input.read_to_end(&mut bytes) => { result?; }
            _ = terminate.recv() => return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted, "Host startup interrupted before password initialization"
            ).into()),
            _ = interrupt.recv() => return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted, "Host startup interrupted before password initialization"
            ).into()),
        }
        if bytes.ends_with(b"\n") {
            bytes.pop();
            if bytes.ends_with(b"\r") {
                bytes.pop();
            }
        }
        let password = String::from_utf8(bytes)
            .map_err(|_| access::AccessError::Invalid("密码必须为 UTF-8。"))?;
        access_service
            .initialize_password(assistant_protocol::SecretValue::new(password))
            .await?;
    }
    // 认证/TLS/绑定失败仍走监听前错误；绝不为展示诊断放松认证或另开端口。
    let configuration = access_service.prepare().await?;
    let enterprise = access_handle.center().is_some();
    let endpoint = instance.bind_and_publish(&configuration).await?;
    let mut supervisor = HostSupervisor::new(SHUTDOWN_TIMEOUT);
    let shutdown = supervisor.shutdown_handle();
    let terminals = user_terminal::UserTerminalService::new();
    let mut state = HttpState::starting(
        HttpEndpointState::new(
            endpoint.access_token(),
            endpoint.authority(),
            endpoint.base_url(),
            endpoint.instance_id().into(),
        ),
        shutdown,
        access_handle,
        terminals.handle.clone(),
    );
    let (mut domains, handle) = crate::user_domain::UserDomainService::new(
        config,
        state.access.credentials.clone(),
        state.startup.clone(),
        enterprise,
    );
    domains.shared.protected_files = state.access.protected_files.clone();
    state.domains = Some(handle);
    println!(
        "EZ Assistant Runtime is listening at {} (discovery: {})",
        endpoint.base_url(),
        endpoint.discovery_path().display()
    );
    let server = RuntimeServer::new(endpoint, state.clone());
    supervisor.spawn_subsystem("host_http", FailurePolicy::ShutdownHost, move |shutdown| {
        server.serve_until(shutdown)
    });
    let access_state = state.clone();
    supervisor.spawn_subsystem("host_access", FailurePolicy::Degrade, move |shutdown| {
        access_service.run_until(access_state, shutdown)
    });
    supervisor.spawn_subsystem("user_terminals", FailurePolicy::Degrade, move |shutdown| {
        terminals.run_until(shutdown)
    });
    if enterprise {
        state.startup.identity_ready();
        let credentials = state.access.credentials.clone();
        supervisor.spawn_subsystem(
            "center_identity",
            FailurePolicy::ShutdownHost,
            move |shutdown| async move {
                credentials.recheck_connections(shutdown).await;
                Ok::<(), std::io::Error>(())
            },
        );
    }
    supervisor.spawn_subsystem(
        "user_domains",
        FailurePolicy::ShutdownHost,
        move |shutdown| domains.run_until(shutdown),
    );
    supervisor
        .run_until(async move {
            tokio::select! { _ = terminate.recv() => {}, _ = interrupt.recv() => {} }
            Ok::<(), std::io::Error>(())
        })
        .await?;
    Ok(())
}
