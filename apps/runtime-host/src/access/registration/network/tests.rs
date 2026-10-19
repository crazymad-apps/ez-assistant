use super::*;
use assistant_protocol::SecretValue;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

fn target(port: u16) -> Target {
    Target {
        client: CenterClient::new(format!("http://127.0.0.1:{port}")).unwrap(),
        center_id: "11111111-1111-4111-8111-111111111111".into(),
        enrollment: Some(Enrollment {
            center_id: "11111111-1111-4111-8111-111111111111".into(),
            id: "device".into(),
            secret: SecretValue::new("a".repeat(64)),
            name: "test".into(),
            platform: "test".into(),
            host_version: "test".into(),
            intent: Intent::Publish,
        }),
    }
}
async fn info(listener: &TcpListener) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        socket.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    assert!(
        !String::from_utf8(bytes)
            .unwrap()
            .to_ascii_lowercase()
            .contains("authorization:")
    );
    let body = r#"{"center_id":"11111111-1111-4111-8111-111111111111","protocol_version":3,"min_protocol_version":3}"#;
    socket
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}
async fn handshake(
    listener: TcpListener,
) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
    info(&listener).await;
    let (tcp, _) = listener.accept().await.unwrap();
    let mut ws = accept_async(tcp).await.unwrap();
    ws.send(Message::Text(serde_json::json!({"type":"hello", "protocol_version":2, "center_id":"11111111-1111-4111-8111-111111111111", "registration_id":"device"}).to_string().into())).await.unwrap();
    assert!(matches!(ws.next().await, Some(Ok(Message::Text(_)))));
    ws.send(Message::Text(
        r#"{"type":"registration_changed","protocol_version":2}"#.into(),
    ))
    .await
    .unwrap();
    ws
}

#[test]
fn address_and_backoff_preserve_contract() {
    assert_eq!(
        connection::endpoint("https://example.test/prefix/").unwrap(),
        "wss://example.test/prefix/api/host-network/connect"
    );
    assert_eq!(
        connection::endpoint("http://example.test").unwrap(),
        "ws://example.test/api/host-network/connect"
    );
    assert_eq!(
        (0..8).map(backoff).collect::<Vec<_>>(),
        [1, 2, 4, 8, 16, 30, 30, 30]
    );
    assert_eq!(backoff(u32::MAX), 30);
}

#[tokio::test]
async fn valid_ping_is_answered_and_owner_cancellation_releases_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = target(listener.local_addr().unwrap().port());
    let network = Network::new(Arc::new(crate::access::Credentials::new()));
    network.publish(Some(target));
    let shutdown = CancellationToken::new();
    let runner = tokio::spawn(network.clone().run_until(shutdown.clone(), None));
    let mut ws = handshake(listener).await;
    ws.send(Message::Ping(vec![1; 8].into())).await.unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap(),
        Some(Ok(Message::Pong(_)))
    ));
    assert_eq!(*network.status.borrow(), HostNetworkState::Online);
    assert_eq!(
        network
            .connected
            .borrow()
            .as_ref()
            .unwrap()
            .self_device_id
            .as_deref(),
        Some("device")
    );
    shutdown.cancel();
    runner.await.unwrap().unwrap();
    assert_eq!(*network.status.borrow(), HostNetworkState::Disconnected);
    assert!(!matches!(
        tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap(),
        Some(Ok(Message::Text(_)))
    ));
}

#[tokio::test]
async fn silent_center_times_out_even_when_notifications_arrive() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = target(listener.local_addr().unwrap().port());
    let network = Network::new(Arc::new(crate::access::Credentials::new()));
    let task_network = network.clone();
    let runner = tokio::spawn(async move { connection::run(&target, &task_network, None).await });
    let mut ws = handshake(listener).await;
    let mut status = network.status.subscribe();
    while *status.borrow_and_update() != HostNetworkState::Online {
        status.changed().await.unwrap();
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(30)).await;
    ws.send(Message::Text(
        r#"{"type":"registration_changed","protocol_version":2}"#.into(),
    ))
    .await
    .unwrap();
    tokio::time::advance(Duration::from_secs(15)).await;
    let (state, stable) = runner.await.unwrap();
    assert_eq!(state, HostNetworkState::Retrying);
    assert!(!stable);
}

#[tokio::test]
async fn incompatible_hello_never_becomes_online() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = target(listener.local_addr().unwrap().port());
    let network = Network::new(Arc::new(crate::access::Credentials::new()));
    let cloned = network.clone();
    let runner = tokio::spawn(async move { connection::run(&target, &cloned, None).await });
    info(&listener).await;
    let (tcp, _) = listener.accept().await.unwrap();
    let mut ws = accept_async(tcp).await.unwrap();
    ws.send(Message::Text(
        r#"{"type":"hello","protocol_version":99,"center_id":"11111111-1111-4111-8111-111111111111","registration_id":"device"}"#
            .into(),
    ))
    .await
    .unwrap();
    assert_eq!(runner.await.unwrap().0, HostNetworkState::Incompatible);
    assert_ne!(*network.status.borrow(), HostNetworkState::Online);
}

#[tokio::test]
async fn blocked_pong_write_has_its_own_five_second_deadline() {
    use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};
    let (client, server) = tokio::io::duplex(8);
    let ws = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
    let mut server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
    let network = Network::new(Arc::new(crate::access::Credentials::new()));
    let runner = tokio::spawn(async move { connection::drive(ws, &network, None, None).await });
    server.send(Message::Ping(vec![1; 8].into())).await.unwrap();
    // 对端不读取 Pong，8 字节管道不足以容纳完整控制帧，写 future 必须进入 Pending。
    tokio::time::sleep(Duration::from_millis(10)).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(5)).await;
    assert_eq!(runner.await.unwrap(), Err(HostNetworkState::Retrying));
}

#[tokio::test]
async fn supervisor_abort_clears_projection_and_releases_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let network = Network::new(Arc::new(crate::access::Credentials::new()));
    network.publish(Some(target(listener.local_addr().unwrap().port())));
    let runner = tokio::spawn(network.clone().run_until(CancellationToken::new(), None));
    let mut ws = handshake(listener).await;
    let mut status = network.subscribe();
    while *status.borrow_and_update() != HostNetworkState::Online {
        status.changed().await.unwrap();
    }
    runner.abort();
    assert!(runner.await.unwrap_err().is_cancelled());
    assert_eq!(*network.status.borrow(), HostNetworkState::Disconnected);
    let message = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap();
    assert!(message.is_none_or(|message| message.is_err()));
}

#[tokio::test]
async fn abrupt_transport_close_retries_instead_of_freezing_as_incompatible() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = target(listener.local_addr().unwrap().port());
    let network = Network::new(Arc::new(crate::access::Credentials::new()));
    let runner = tokio::spawn(async move { connection::run(&target, &network, None).await });
    let ws = handshake(listener).await;
    drop(ws);
    let (state, _) = tokio::time::timeout(Duration::from_secs(2), runner)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state, HostNetworkState::Retrying);
}
