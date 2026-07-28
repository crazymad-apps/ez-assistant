//! 出向调试推送客户端。
//!
//! fire-and-forget 语义：`post` 只做有界队列的非阻塞入队，HTTP 发送由独立
//! 后台任务串行执行，连接与请求都有短超时。队列满、发送失败或通道关闭时
//! 一次性打印到 stderr 并自关闭，绝不影响主流程（对话、Run、调度）。
//! 推送端不读响应正文。

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::sync::mpsc;

use crate::wire::{DebugChannel, DebugEnvelope, DebugPayload, now_ms};

/// 推送队列容量；viewer 消费不过来时多余消息直接丢弃，入队方不等待。
const QUEUE_CAPACITY: usize = 256;

/// viewer 连接超时；loopback 下 1 秒已足够。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// 单次推送总超时；viewer 卡顿时宁可失败自静音，绝不拖住主流程。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// 向 viewer server `POST /ingest` 推送调试信封的客户端。
///
/// 构造时需处于 Tokio runtime 上下文（后台发送任务随即 spawn）。
pub struct DebugClient {
    tx: mpsc::Sender<DebugEnvelope>,
    correlation_id: Option<String>,
    seq: AtomicU64,
    disabled: Arc<AtomicBool>,
}

impl DebugClient {
    /// `base_url` 形如 `http://localhost:7331`；实际推送到 `{base_url}/ingest`。
    pub fn new(base_url: impl AsRef<str>) -> Self {
        let ingest_url = format!("{}/ingest", base_url.as_ref().trim_end_matches('/'));
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("static reqwest client config must build");
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        let disabled = Arc::new(AtomicBool::new(false));
        tokio::spawn(send_loop(http, ingest_url, rx, Arc::clone(&disabled)));
        Self {
            tx,
            correlation_id: None,
            seq: AtomicU64::new(0),
            disabled,
        }
    }

    /// 附加关联 ID，对应 `TraceContext::correlation_id`。
    pub fn with_correlation_id(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }

    /// 推送一条 `llm` 通道 payload；兼容既有模型调试调用方。
    pub fn post(&self, payload: DebugPayload) {
        self.post_on(DebugChannel::Llm, payload);
    }

    /// 在指定通道推送 payload；非阻塞入队，队列满或已静音时直接丢弃。
    pub fn post_on(&self, channel: DebugChannel, payload: DebugPayload) {
        if self.disabled.load(Ordering::Relaxed) {
            return;
        }
        let envelope = DebugEnvelope {
            ch: channel,
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            sent_at_ms: now_ms(),
            correlation_id: self.correlation_id.clone(),
            payload,
        };
        if self.tx.try_send(envelope).is_err() {
            // 队列满（viewer 消费过慢）或发送任务已退出：一次性告警后自静音。
            mute_once(&self.disabled, "viewer 消费过慢或推送通道已关闭");
        }
    }

    /// 是否已因失败自静音（测试与诊断用）。
    pub fn is_muted(&self) -> bool {
        self.disabled.load(Ordering::Relaxed)
    }
}

/// 串行消费队列并逐个 POST；任何发送失败都自静音并退出（通道随之关闭，
/// 后续 `post` 直接丢弃）。`DebugClient` 被 drop 后通道关闭，任务随之退出。
async fn send_loop(
    http: reqwest::Client,
    ingest_url: String,
    mut rx: mpsc::Receiver<DebugEnvelope>,
    disabled: Arc<AtomicBool>,
) {
    while let Some(envelope) = rx.recv().await {
        let failed = match http.post(&ingest_url).json(&envelope).send().await {
            Ok(response) if response.status().is_success() => None,
            Ok(response) => Some(format!("viewer 返回 {}", response.status())),
            Err(error) => Some(format!("无法连接 viewer：{error}")),
        };
        if let Some(reason) = failed {
            mute_once(&disabled, &reason);
            return;
        }
    }
}

/// 置为静音并一次性告警；多触发源并发时只打印一次。
fn mute_once(disabled: &AtomicBool, reason: &str) {
    if !disabled.swap(true, Ordering::Relaxed) {
        eprintln!("[debug] 推送失败（{reason}），后续调试消息已静音");
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    /// viewer 接受连接但永不响应：主流程的 `post` 不阻塞，队列打满后自静音。
    #[tokio::test]
    async fn unresponsive_viewer_never_blocks_the_caller() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let mut held = Vec::new();
            // 持有连接，永不写回响应。
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });

        let client = DebugClient::new(format!("http://{addr}")).with_correlation_id("test");
        // 同步入队远超队列容量的消息：整个循环不等待 viewer，打满即静音。
        for _ in 0..QUEUE_CAPACITY * 4 {
            client.post(DebugPayload::EstablishmentFailed {
                error: "probe".to_owned(),
            });
        }
        assert!(client.is_muted());
        // 静音后的推送直接丢弃，不 panic 也不阻塞。
        client.post(DebugPayload::EstablishmentFailed {
            error: "probe".to_owned(),
        });
    }
}
