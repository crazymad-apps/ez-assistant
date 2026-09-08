//! Host 唯一 HTTP／HTTPS 监听的生命周期。本机与其他设备共用端口和路由。

use crate::{
    access::AccessError,
    endpoint::OwnedEndpoint,
    http::{HttpState, router},
};
use assistant_protocol::{HostAccessConfiguration, HostAccessScheme};
use axum_server::{Handle, tls_rustls::RustlsConfig};
use std::{
    net::{Ipv4Addr, SocketAddr, TcpListener},
    time::Duration,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub(crate) struct RuntimeServer {
    endpoint: OwnedEndpoint,
    state: HttpState,
}

impl RuntimeServer {
    pub(crate) fn new(endpoint: OwnedEndpoint, state: HttpState) -> Self {
        Self { endpoint, state }
    }

    pub(crate) async fn serve_until(
        mut self,
        shutdown: CancellationToken,
    ) -> Result<(), AccessError> {
        let listener = self.endpoint.take_listener();
        let tls = self.endpoint.take_tls();
        let handle = Handle::new();
        // ConnectInfo 来自实际 TCP peer；不能用 Host / Forwarded / Origin 冒充本机来源。
        let service = router(self.state).into_make_service_with_connect_info::<SocketAddr>();
        let task = match tls {
            Some(tls) => {
                let server = axum_server::from_tcp_rustls(listener, tls)
                    .map_err(|_| AccessError::Unavailable)?
                    .handle(handle.clone());
                tokio::spawn(async move { server.serve(service).await })
            }
            None => {
                let server = axum_server::from_tcp(listener)
                    .map_err(|_| AccessError::Unavailable)?
                    .handle(handle.clone());
                tokio::spawn(async move { server.serve(service).await })
            }
        };
        let mut active = ActiveListener {
            handle,
            task: Some(task),
        };
        let result = tokio::select! {
            () = shutdown.cancelled() => Ok(()),
            result = active.finished() => result,
        };
        active.stop().await;
        result
    }
}

pub(crate) fn bind(port: u16) -> Result<TcpListener, AccessError> {
    let bind_error = |error: std::io::Error| {
        if error.kind() == std::io::ErrorKind::AddrInUse {
            AccessError::PortInUse(port)
        } else {
            AccessError::Invalid("无法监听 Host 端口，请检查端口和系统权限。")
        }
    };
    // macOS 允许 wildcard 与已有 loopback 监听重用同端口；先检查精确本机地址，
    // 避免 discovery 指向别的服务。保留 SO_REUSEADDR 以支持正常重启后的 TIME_WAIT。
    drop(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(bind_error)?);
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).map_err(bind_error)?;
    listener
        .set_nonblocking(true)
        .map_err(|_| AccessError::Unavailable)?;
    Ok(listener)
}

/// 同一协议用于所有客户端；仅 HTTPS 读取证书，不引入协议嗅探或第二个端口。
pub(crate) async fn tls_configuration(
    configuration: &HostAccessConfiguration,
) -> Result<Option<RustlsConfig>, AccessError> {
    if configuration.scheme == HostAccessScheme::Http {
        return Ok(None);
    }
    let certificate = configuration
        .tls_certificate
        .as_ref()
        .ok_or(AccessError::Unavailable)?;
    let key = configuration
        .tls_private_key
        .as_ref()
        .ok_or(AccessError::Unavailable)?;
    RustlsConfig::from_pem_file(certificate, key)
        .await
        .map(Some)
        .map_err(|_| AccessError::Invalid("无法加载 HTTPS 证书或私钥。"))
}

// 唯一监听的任务由 RuntimeServer 持有；正常关闭限时等待，Supervisor abort 时 Drop 也会回收。
struct ActiveListener {
    handle: Handle<SocketAddr>,
    task: Option<JoinHandle<Result<(), std::io::Error>>>,
}
impl ActiveListener {
    async fn finished(&mut self) -> Result<(), AccessError> {
        let task = self.task.as_mut().ok_or(AccessError::Unavailable)?;
        let result = task.await;
        self.task = None;
        result
            .map_err(|_| AccessError::Unavailable)?
            .map_err(|_| AccessError::Unavailable)
    }
    async fn stop(mut self) {
        self.handle.graceful_shutdown(Some(Duration::from_secs(3)));
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(Duration::from_secs(4), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}
impl Drop for ActiveListener {
    fn drop(&mut self) {
        self.handle.shutdown();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
