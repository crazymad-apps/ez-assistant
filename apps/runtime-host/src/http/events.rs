//! Runtime broadcast 到 SSE 的可丢弃观察投影。

use std::{convert::Infallible, time::Duration};

use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
};
use futures_core::Stream;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::HttpState;

pub(super) async fn stream_events(
    State(state): State<HttpState>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    Sse::new(project_events(
        state.runtime.subscribe_event_envelopes(),
        state.device_gateway.subscribe_events(),
        state.shutdown,
    ))
    .keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// 把可丢弃 Runtime 广播投影为 SSE。订阅者一旦落后，就不能继续假装事件连续：先发送
/// Host 私有的 gap 控制事件，再关闭本次流，迫使客户端重连并从权威快照恢复。
fn project_events(
    mut runtime_receiver: broadcast::Receiver<assistant_protocol::RuntimeEventEnvelope>,
    mut device_receiver: broadcast::Receiver<assistant_protocol::DeviceGatewayEvent>,
    shutdown: CancellationToken,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let stream = async_stream::stream! {
        // 立即写出一个无业务语义的帧，避免 WebView 等待 keep-alive 首字节后才认为
        // SSE 已建立。客户端会忽略 comment，但仍能保证先订阅事件、再读取快照。
        yield Ok(Event::default().comment("connected"));
        loop {
            let received = tokio::select! {
                () = shutdown.cancelled() => break,
                received = runtime_receiver.recv() => EventSource::Runtime(received.map(Box::new)),
                received = device_receiver.recv() => EventSource::Device(received),
            };
            match received {
                EventSource::Runtime(Ok(event)) => {
                    let Ok(data) = serde_json::to_string(&event) else {
                        break;
                    };
                    yield Ok(Event::default().event("runtime_event").data(data));
                }
                EventSource::Device(Ok(event)) => {
                    let Ok(data) = serde_json::to_string(&event) else {
                        break;
                    };
                    yield Ok(Event::default().event("device_gateway_event").data(data));
                }
                EventSource::Runtime(Err(broadcast::error::RecvError::Lagged(dropped_events)))
                | EventSource::Device(Err(broadcast::error::RecvError::Lagged(dropped_events))) => {
                    yield Ok(Event::default()
                        .event("stream_gap")
                        .data(stream_gap_data(dropped_events)));
                    break;
                }
                EventSource::Runtime(Err(broadcast::error::RecvError::Closed))
                | EventSource::Device(Err(broadcast::error::RecvError::Closed)) => break,
            }
        }
    };
    stream
}

/// 合并到同一 SSE 连接的事件来源及其各自广播接收结果。
///
/// 任一来源发生 lag 都会关闭整条流，要求 Desktop 重新读取对应权威快照。
enum EventSource {
    Runtime(Result<Box<assistant_protocol::RuntimeEventEnvelope>, broadcast::error::RecvError>),
    Device(Result<assistant_protocol::DeviceGatewayEvent, broadcast::error::RecvError>),
}

fn stream_gap_data(dropped_events: u64) -> String {
    serde_json::json!({ "dropped_events": dropped_events }).to_string()
}

#[cfg(test)]
mod tests {
    use assistant_protocol::{RuntimeEvent, RuntimeEventEnvelope};
    use axum::{body::to_bytes, response::IntoResponse};
    use futures_util::StreamExt;

    use super::*;

    #[tokio::test]
    async fn new_subscriber_gets_an_immediate_connection_frame() {
        let (_runtime_sender, runtime_receiver) = broadcast::channel(1);
        let (_device_sender, device_receiver) = broadcast::channel(1);
        let shutdown = CancellationToken::new();
        shutdown.cancel();

        let response =
            Sse::new(project_events(runtime_receiver, device_receiver, shutdown)).into_response();
        let body = to_bytes(response.into_body(), 1_024)
            .await
            .expect("SSE body");
        let body = String::from_utf8(body.to_vec()).expect("UTF-8 SSE");

        assert_eq!(body, ": connected\n\n");
    }

    #[tokio::test]
    async fn lagged_subscriber_gets_a_gap_event_and_then_disconnects() {
        let (sender, runtime_receiver) = broadcast::channel(1);
        let (_device_sender, device_receiver) = broadcast::channel(1);
        sender.send(envelope(1)).expect("first event");
        sender.send(envelope(2)).expect("second event");

        let response = Sse::new(project_events(
            runtime_receiver,
            device_receiver,
            CancellationToken::new(),
        ))
        .into_response();
        let body = to_bytes(response.into_body(), 1_024)
            .await
            .expect("SSE body");
        let body = String::from_utf8(body.to_vec()).expect("UTF-8 SSE");

        assert!(body.contains("event: stream_gap\n"));
        assert!(body.contains("data: {\"dropped_events\":1}\n\n"));
    }

    #[test]
    fn gap_payload_only_contains_the_dropped_event_count() {
        assert_eq!(stream_gap_data(3), r#"{"dropped_events":3}"#);
    }

    #[tokio::test]
    async fn device_gateway_change_is_projected_as_an_invalidation_event() {
        let (_runtime_sender, runtime_receiver) = broadcast::channel(1);
        let (device_sender, device_receiver) = broadcast::channel(1);
        device_sender
            .send(assistant_protocol::DeviceGatewayEvent::Changed)
            .expect("device event");
        let response = Sse::new(
            project_events(runtime_receiver, device_receiver, CancellationToken::new()).take(2),
        )
        .into_response();
        let body = to_bytes(response.into_body(), 1_024)
            .await
            .expect("SSE body");
        let body = String::from_utf8(body.to_vec()).expect("UTF-8 SSE");

        assert!(body.contains("event: device_gateway_event\n"));
        assert!(body.contains("data: {\"type\":\"changed\"}\n\n"));
    }

    fn envelope(sequence: u64) -> RuntimeEventEnvelope {
        RuntimeEventEnvelope {
            sequence,
            emitted_at_ms: 1,
            event: RuntimeEvent::RuntimeShuttingDown,
        }
    }
}
