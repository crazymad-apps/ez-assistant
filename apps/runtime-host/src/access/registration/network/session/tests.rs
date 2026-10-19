use super::*;
#[tokio::test(start_paused = true)]
async fn source_renewal_timeout_closes_only_its_scope() {
    let (send, mut outgoing) = mpsc::channel(16);
    let (session, _receive) = Session::new(send, Arc::new(Credentials::new()), None);
    let permit = AccessPermit::new(false, None, CancellationToken::new());
    let source = Source {
        view_id: None,
        permit: permit.clone(),
        device: "device".into(),
        capability: HostNetworkCapability::Ordinary,
        cancelled: CancellationToken::new(),
        ready: watch::channel(Some(Ok(()))).0,
    };
    session.install_source("access", source.clone()).unwrap();
    let guard = LeaseGuard {
        session: session.clone(),
        id: "access".into(),
    };
    let client = CenterClient::new("http://127.0.0.1:1".into()).unwrap();
    let task = tokio::spawn(session.clone().work(
        Work::Source(
            "access".into(),
            Box::new(source.clone()),
            guard,
            Instant::now(),
        ),
        client,
        "center".into(),
        None,
    ));
    let Message::Text(request) = outgoing.recv().await.unwrap() else {
        panic!("expected request")
    };
    let value: Value = serde_json::from_str(&request).unwrap();
    assert_eq!(value["action"], "renew_source");
    session.receive(json!({"type":"response", "request_id":value["request_id"], "result":{"valid_for_ms":45000}}), false).unwrap();
    let Message::Text(request) = outgoing.recv().await.unwrap() else {
        panic!("expected renewal")
    };
    assert_eq!(
        serde_json::from_str::<Value>(&request).unwrap()["action"],
        "renew_source"
    );
    task.await.unwrap(); // 自动推进到该请求的 5 秒超时。
    assert!(source.cancelled.is_cancelled());
    assert!(session.sources.lock().unwrap().is_empty());
    assert!(permit.check().is_ok());
    assert!(session.pending.lock().unwrap().is_empty());
}
#[tokio::test]
async fn dropped_connection_cancels_pending_request_and_access() {
    let (send, mut outgoing) = mpsc::channel(16);
    let (session, _receive) = Session::new(send, Arc::new(Credentials::new()), None);
    let request = {
        let session = session.clone();
        tokio::spawn(async move { session.request("directory", json!({})).await })
    };
    outgoing.recv().await.unwrap();
    let source = Source {
        view_id: None,
        permit: AccessPermit::new(false, None, CancellationToken::new()),
        device: "device".into(),
        capability: HostNetworkCapability::Ordinary,
        cancelled: CancellationToken::new(),
        ready: watch::channel(Some(Ok(()))).0,
    };
    session.install_source("access", source.clone()).unwrap();
    session.close();
    assert!(request.await.unwrap().is_err());
    assert!(source.cancelled.is_cancelled());
    assert!(session.pending.lock().unwrap().is_empty());
    assert!(session.sources.lock().unwrap().is_empty());
}

#[tokio::test]
async fn concurrent_open_waiters_share_access_and_cancel_independently() {
    let (send, _outgoing) = mpsc::channel(16);
    let (session, mut jobs) = Session::new(send, Arc::new(Credentials::new()), None);
    let permit = AccessPermit::new(false, None, CancellationToken::new());
    let open = |session: Arc<Session>, permit: AccessPermit| {
        tokio::spawn(async move {
            session
                .open_source(
                    "device".into(),
                    HostNetworkCapability::Ordinary,
                    permit,
                    true,
                )
                .await
        })
    };
    let first = open(session.clone(), permit.clone());
    let Work::Opening(id, source, guard) = jobs.recv().await.unwrap() else {
        panic!("opening expected")
    };
    let second = open(session.clone(), permit);
    tokio::task::yield_now().await;
    assert!(jobs.try_recv().is_err());
    first.abort();
    let _ = first.await;
    assert_eq!(source.ready.receiver_count(), 1);
    source.ready.send_replace(Some(Ok(())));
    let (other, _) = second.await.unwrap().unwrap();
    assert_eq!(other, id);
    assert!(!source.cancelled.is_cancelled());
    drop(guard);
    assert!(source.cancelled.is_cancelled());
    assert!(session.sources.lock().unwrap().is_empty());
}

#[tokio::test]
async fn late_open_after_access_ended_does_not_close_device_connection() {
    let (send, mut outgoing) = mpsc::channel(16);
    let (session, mut jobs) = Session::new(send, Arc::new(Credentials::new()), None);
    session.receive(json!({"type":"stream_open","access_id":"00112233-4455-4677-8899-aabbccddeeff","stream_id":"1","kind":"http"}), true).unwrap();
    let Message::Text(reply) = outgoing.recv().await.unwrap() else {
        panic!("expected reset")
    };
    assert_eq!(
        serde_json::from_str::<Value>(&reply).unwrap()["type"],
        "stream_reset"
    );
    assert!(!session.ended.is_cancelled());
    assert!(jobs.try_recv().is_err());
}
