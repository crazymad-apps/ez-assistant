//! MCP 初始化在 Host 就绪后运行；Supervisor 持有任务，关闭时取消并回收。

use assistant_protocol::RuntimeEvent;
use assistant_runtime::{AssistantRuntime, RuntimeResult};
use std::{sync::Arc, time::Instant};
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(
    runtime: Arc<AssistantRuntime>,
    shutdown: CancellationToken,
) -> RuntimeResult<()> {
    let mut events = runtime.subscribe_events();
    let started = Instant::now();
    tokio::select! {
        biased;
        () = shutdown.cancelled() => return Ok(()),
        result = runtime.bootstrap_mcp() => {
            eprintln!("runtime-host: MCP initialization elapsed_ms={} success={}", started.elapsed().as_millis(), result.is_ok());
        }
    }
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            event = events.recv() => {
                match event {
                    Ok(RuntimeEvent::PluginCatalogChanged) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let _ = runtime.bootstrap_mcp().await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Ok(_) => {}
                }
            }
        }
    }
    Ok(())
}
