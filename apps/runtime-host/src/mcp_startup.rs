//! MCP 初始化在 Host 就绪后运行；Supervisor 持有任务，关闭时取消并回收。

use assistant_runtime::{AssistantRuntime, RuntimeResult};
use std::{sync::Arc, time::Instant};
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(
    runtime: Arc<AssistantRuntime>,
    shutdown: CancellationToken,
) -> RuntimeResult<()> {
    let started = Instant::now();
    tokio::select! {
        biased;
        () = shutdown.cancelled() => return Ok(()),
        result = runtime.bootstrap_mcp() => {
            eprintln!("runtime-host: MCP initialization elapsed_ms={} success={}", started.elapsed().as_millis(), result.is_ok());
            result?;
        }
    }
    // 初始化完成后保持子系统生命周期；连接资源由 Runtime/Host factory 在退出时统一回收。
    shutdown.cancelled().await;
    Ok(())
}
