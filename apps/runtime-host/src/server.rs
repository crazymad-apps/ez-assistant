//! HTTP Host 生命周期与 Runtime 受控关闭编排。

use std::{path::PathBuf, sync::Arc};

use assistant_protocol::ShutdownRuntimeRequest;
use assistant_runtime::AssistantRuntime;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    endpoint::OwnedEndpoint,
    http::{HttpState, router},
};

/// 持有本实例发现文件和监听端点，并把请求桥接到唯一 Runtime。
pub(crate) struct RuntimeServer {
    endpoint: OwnedEndpoint,
    runtime: Arc<AssistantRuntime>,
    runtime_home: PathBuf,
}

impl RuntimeServer {
    pub(crate) fn new(
        endpoint: OwnedEndpoint,
        runtime: Arc<AssistantRuntime>,
        runtime_home: PathBuf,
    ) -> Self {
        Self {
            endpoint,
            runtime,
            runtime_home,
        }
    }

    /// 使用 Ctrl-C 作为进程级关闭信号运行 Server。
    pub(crate) async fn serve(self) -> Result<(), ServerError> {
        let shutdown = CancellationToken::new();
        let signal = shutdown.clone();
        let signal_task = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                signal.cancel();
            }
        });
        let result = self.serve_until(shutdown).await;
        signal_task.abort();
        if let Err(error) = signal_task.await
            && !error.is_cancelled()
        {
            return Err(ServerError::SignalTask);
        }
        result
    }

    /// 服务到外部取消或关闭命令；网络连接退出不改变业务 Run 生命周期。
    pub(crate) async fn serve_until(
        mut self,
        shutdown: CancellationToken,
    ) -> Result<(), ServerError> {
        let listener = self.endpoint.take_listener();
        let state = HttpState::new(
            self.runtime.clone(),
            self.endpoint.access_token(),
            self.endpoint.authority(),
            self.endpoint.base_url(),
            self.runtime_home,
            shutdown.clone(),
        );
        let serve_result = axum::serve(listener, router(state))
            .with_graceful_shutdown(shutdown.clone().cancelled_owned())
            .await;

        // 停止接收连接后再关闭 Runtime；发现文件随 self.endpoint 最后释放。
        let runtime_result = self
            .runtime
            .shutdown(ShutdownRuntimeRequest::default())
            .await;
        serve_result.map_err(ServerError::Serve)?;
        runtime_result.map_err(|error| ServerError::Runtime(error.to_string()))?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error("runtime HTTP server failed: {0}")]
    Serve(std::io::Error),
    #[error("runtime signal task failed")]
    SignalTask,
    #[error("runtime controlled shutdown failed: {0}")]
    Runtime(String),
}
