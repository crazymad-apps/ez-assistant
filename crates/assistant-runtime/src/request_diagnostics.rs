//! 按一次宿主请求关联的临时阶段诊断；不保存业务状态、正文或持久追踪记录。

use std::{future::Future, sync::Arc, time::Instant};

use sha2::{Digest, Sha256};

tokio::task_local! {
    static REQUEST: Arc<RequestContext>;
}

struct RequestContext {
    correlation: String,
    started: Instant,
}

/// 仅包含固定阶段名；日志不能由调用参数构造任意字段或内容。
#[derive(Clone, Copy)]
pub enum RequestPhase {
    Request,
    Authentication,
    EnterpriseVerification,
    UserDomain,
    SessionLoad,
    SessionPreparation,
    SessionGate,
    StoreQueue,
    StoreReply,
    Projection,
    Response,
}

impl RequestPhase {
    fn name(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Authentication => "authentication",
            Self::EnterpriseVerification => "enterprise_verification",
            Self::UserDomain => "user_domain",
            Self::SessionLoad => "session_load",
            Self::SessionPreparation => "session_preparation",
            Self::SessionGate => "session_gate",
            Self::StoreQueue => "store_queue",
            Self::StoreReply => "store_reply",
            Self::Projection => "projection",
            Self::Response => "response",
        }
    }
}

/// 阶段离开时记录耗时；离开可能由成功、错误或取消触发，不代表业务成功。
#[must_use]
pub struct RequestPhaseGuard(Option<(Arc<RequestContext>, RequestPhase, Instant)>);

/// 宿主显式开启诊断时，以自己的临时请求身份包围整个 ingress future。
///
/// 身份只输出固定长度摘要；task-local 随 future 释放，不传播到派生的后台执行任务。
pub async fn scope_request_diagnostics<T>(scope: &str, future: impl Future<Output = T>) -> T {
    REQUEST
        .scope(
            Arc::new(RequestContext {
                correlation: correlation_id(scope),
                started: Instant::now(),
            }),
            async {
                let _request = request_phase(RequestPhase::Request);
                future.await
            },
        )
        .await
}

/// 开始一个短期阶段；没有宿主请求作用域时是空操作。
pub fn request_phase(phase: RequestPhase) -> RequestPhaseGuard {
    let context = REQUEST.try_with(Arc::clone).ok();
    RequestPhaseGuard(context.map(|context| {
        eprintln!(
            "runtime-request trace={} stage={} event=begin at_ms={}",
            context.correlation,
            phase.name(),
            context.started.elapsed().as_millis(),
        );
        (context, phase, Instant::now())
    }))
}

/// JSON 解码后关联客户端 request_id，身份核验前生成的 trace 仍保持不变。
pub fn correlate_request_id(request_id: &str) {
    if let Ok(context) = REQUEST.try_with(Arc::clone) {
        eprintln!(
            "runtime-request trace={} request={} event=correlate at_ms={}",
            context.correlation,
            correlation_id(request_id),
            context.started.elapsed().as_millis(),
        );
    }
}

/// 只记录最终 HTTP 状态，不记录错误消息或响应正文。
pub fn record_response_status(status: u16) {
    if let Ok(context) = REQUEST.try_with(Arc::clone) {
        eprintln!(
            "runtime-request trace={} stage=response event=status status={} at_ms={}",
            context.correlation,
            status,
            context.started.elapsed().as_millis(),
        );
    }
}

impl Drop for RequestPhaseGuard {
    fn drop(&mut self) {
        if let Some((context, phase, started)) = &self.0 {
            eprintln!(
                "runtime-request trace={} stage={} event=end elapsed_ms={} at_ms={}",
                context.correlation,
                phase.name(),
                started.elapsed().as_millis(),
                context.started.elapsed().as_millis(),
            );
        }
    }
}

fn correlation_id(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_requests_do_not_share_identity_or_leak_into_background_tasks() {
        let left = scope_request_diagnostics("left\nsecret", async {
            tokio::task::yield_now().await;
            assert!(
                tokio::spawn(async { REQUEST.try_with(Arc::clone).is_err() })
                    .await
                    .unwrap()
            );
            REQUEST.with(|context| context.correlation.clone())
        });
        let right = scope_request_diagnostics("right", async {
            tokio::task::yield_now().await;
            REQUEST.with(|context| context.correlation.clone())
        });
        let (left, right) = tokio::join!(left, right);
        assert_eq!(left, correlation_id("left\nsecret"));
        assert_ne!(left, right);
        assert_eq!(left.len(), 12);
        assert!(left.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(REQUEST.try_with(Arc::clone).is_err());
        assert!(request_phase(RequestPhase::StoreReply).0.is_none());
    }
}
