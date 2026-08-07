//! 单活动客户端 Unix Socket server 生命周期。

mod connection;
mod dispatch;

use std::sync::Arc;

use assistant_protocol::ShutdownRuntimeRequest;
use assistant_runtime::AssistantRuntime;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use self::connection::{ConnectionEnd, serve_connection};
use crate::endpoint::OwnedEndpoint;

/// 持有监听端点，并将每个活动连接桥接到唯一的 Assistant Runtime。
pub(crate) struct RuntimeServer {
    endpoint: OwnedEndpoint,
    runtime: Arc<AssistantRuntime>,
}

impl RuntimeServer {
    pub(crate) fn new(endpoint: OwnedEndpoint, runtime: Arc<AssistantRuntime>) -> Self {
        Self { endpoint, runtime }
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

    /// 接受单个活动客户端，直到外部取消或客户端请求关闭 Runtime。
    pub(crate) async fn serve_until(self, shutdown: CancellationToken) -> Result<(), ServerError> {
        loop {
            let accepted = tokio::select! {
                () = shutdown.cancelled() => break,
                accepted = self.endpoint.listener().accept() => accepted,
            };
            let (stream, _) = accepted.map_err(ServerError::Accept)?;
            match serve_connection(self.runtime.clone(), stream, shutdown.clone())
                .await
                .map_err(|_| ServerError::ConnectionTask)?
            {
                ConnectionEnd::Disconnected => {}
                ConnectionEnd::ShutdownRequested => {
                    shutdown.cancel();
                    break;
                }
            }
        }

        // Endpoint 停止接收连接后再关闭 Runtime，确保业务生命周期只有一个权威出口。
        self.runtime
            .shutdown(ShutdownRuntimeRequest::default())
            .await
            .map_err(|error| ServerError::Runtime(error.to_string()))?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error("runtime endpoint accept failed: {0}")]
    Accept(std::io::Error),
    #[error("runtime connection task failed")]
    ConnectionTask,
    #[error("runtime signal task failed")]
    SignalTask,
    #[error("runtime controlled shutdown failed: {0}")]
    Runtime(String),
}

#[cfg(test)]
mod tests;
