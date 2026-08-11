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
        state.runtime.subscribe_events(),
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
    mut receiver: broadcast::Receiver<assistant_protocol::RuntimeEvent>,
    shutdown: CancellationToken,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let stream = async_stream::stream! {
        loop {
            let received = tokio::select! {
                () = shutdown.cancelled() => break,
                received = receiver.recv() => received,
            };
            match received {
                Ok(event) => {
                    let Ok(data) = serde_json::to_string(&event) else {
                        break;
                    };
                    yield Ok(Event::default().event("runtime_event").data(data));
                }
                Err(broadcast::error::RecvError::Lagged(dropped_events)) => {
                    yield Ok(Event::default()
                        .event("stream_gap")
                        .data(stream_gap_data(dropped_events)));
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    stream
}

fn stream_gap_data(dropped_events: u64) -> String {
    serde_json::json!({ "dropped_events": dropped_events }).to_string()
}

#[cfg(test)]
mod tests {
    use assistant_protocol::RuntimeEvent;
    use axum::{body::to_bytes, response::IntoResponse};

    use super::*;

    #[tokio::test]
    async fn lagged_subscriber_gets_a_gap_event_and_then_disconnects() {
        let (sender, receiver) = broadcast::channel(1);
        sender
            .send(RuntimeEvent::RuntimeShuttingDown)
            .expect("first event");
        sender
            .send(RuntimeEvent::RuntimeShuttingDown)
            .expect("second event");

        let response = Sse::new(project_events(receiver, CancellationToken::new())).into_response();
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
}
