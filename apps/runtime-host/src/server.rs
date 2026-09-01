//! Desktop HTTP 子系统生命周期；进程级观察与 Runtime 关闭由 HostSupervisor 编排。

use std::{path::PathBuf, sync::Arc};

use assistant_runtime::AssistantRuntime;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    device::DeviceGatewayHandle,
    endpoint::OwnedEndpoint,
    http::{HttpEndpointState, HttpState, router},
    speech::SpeechServiceHandle,
};

/// 持有本实例发现文件和监听端点，并把请求桥接到唯一 Runtime。
pub(crate) struct RuntimeServer {
    endpoint: OwnedEndpoint,
    runtime: Arc<AssistantRuntime>,
    runtime_home: PathBuf,
    device_gateway: DeviceGatewayHandle,
    speech: SpeechServiceHandle,
}

impl RuntimeServer {
    pub(crate) fn new(
        endpoint: OwnedEndpoint,
        runtime: Arc<AssistantRuntime>,
        runtime_home: PathBuf,
        device_gateway: DeviceGatewayHandle,
        speech: SpeechServiceHandle,
    ) -> Self {
        Self {
            endpoint,
            runtime,
            runtime_home,
            device_gateway,
            speech,
        }
    }

    /// 服务到子系统取消；Host Command 可以通过根令牌请求整个进程关闭。
    pub(crate) async fn serve_until(
        mut self,
        subsystem_shutdown: CancellationToken,
        host_shutdown: CancellationToken,
    ) -> Result<(), ServerError> {
        let listener = self.endpoint.take_listener();
        let endpoint_state = HttpEndpointState::new(
            self.endpoint.access_token(),
            self.endpoint.authority(),
            self.endpoint.base_url(),
            self.runtime_home,
        );
        let state = HttpState::new(
            self.runtime.clone(),
            endpoint_state,
            self.device_gateway,
            self.speech,
            host_shutdown,
        );
        let serve_result = axum::serve(listener, router(state))
            .with_graceful_shutdown(subsystem_shutdown.cancelled_owned())
            .await;
        serve_result.map_err(ServerError::Serve)?;
        Ok(())
    }
}

/// Desktop HTTP 长期子系统的启动或服务错误。
#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error("runtime HTTP server failed: {0}")]
    Serve(std::io::Error),
}
