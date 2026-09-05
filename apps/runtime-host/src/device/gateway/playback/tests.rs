use super::*;

fn output() -> PlaybackOutput {
    PlaybackOutput {
        output_id: "output".to_owned(),
        run_id: assistant_protocol::RunId::new("run").unwrap(),
        text: "播报".to_owned(),
        pcm: std::sync::Arc::from([0_u8; 640]),
    }
}

async fn prepared() -> (
    PreparedPlayback,
    mpsc::Receiver<ConnectionCommand>,
    CancellationToken,
) {
    let (tx, mut rx) = mpsc::channel(2);
    let token = CancellationToken::new();
    let mut reserving = Box::pin(PreparedPlayback::reserve(
        tx,
        "output".to_owned(),
        token.clone(),
    ));
    assert!(futures_util::poll!(reserving.as_mut()).is_pending());
    let Some(ConnectionCommand::PreparePlayback(request)) = rx.recv().await else {
        panic!("prepare command")
    };
    assert!(!request.cancellation.is_cancelled());
    request
        .response
        .send(PlaybackPreparationResult::Accepted)
        .unwrap_or_else(|_| panic!("response"));
    (reserving.await.unwrap(), rx, token)
}

#[tokio::test]
async fn channel_enqueue_is_not_queue_acceptance_and_ack_transfers_ownership() {
    let (prepared, mut commands, token) = prepared().await;
    let pcm = output();
    let weak = std::sync::Arc::downgrade(&pcm.pcm);
    let mut attaching = Box::pin(prepared.attach(pcm));
    assert!(futures_util::poll!(attaching.as_mut()).is_pending());
    let Some(ConnectionCommand::StartPlayback { output, response }) = commands.recv().await else {
        panic!("start command")
    };
    assert!(futures_util::poll!(attaching.as_mut()).is_pending());
    response.send(true).unwrap();
    assert_eq!(attaching.await, Ok(()));
    assert!(!token.is_cancelled());
    assert!(weak.upgrade().is_some());
    drop(output);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn ptt_between_prepare_and_attach_reports_cancelled_without_sending_audio() {
    let (prepared, mut commands, token) = prepared().await;
    token.cancel();
    assert_eq!(
        prepared.attach(output()).await,
        Err(ChannelOutputDispatchError::Cancelled)
    );
    assert!(commands.try_recv().is_err());
}

#[tokio::test]
async fn rejected_or_disconnected_attach_never_reports_success() {
    for rejected in [true, false] {
        let (prepared, mut commands, token) = prepared().await;
        let mut attaching = Box::pin(prepared.attach(output()));
        assert!(futures_util::poll!(attaching.as_mut()).is_pending());
        let Some(ConnectionCommand::StartPlayback { response, .. }) = commands.recv().await else {
            panic!("start command")
        };
        if rejected {
            response.send(false).unwrap();
        } else {
            drop(response);
        }
        assert_eq!(
            attaching.await,
            Err(if rejected {
                ChannelOutputDispatchError::Cancelled
            } else {
                ChannelOutputDispatchError::Unavailable
            })
        );
        assert!(token.is_cancelled());
    }
}

#[tokio::test]
async fn prepare_timeout_cancels_even_a_late_reservation() {
    let (tx, mut commands) = mpsc::channel(2);
    let token = CancellationToken::new();
    let result = PreparedPlayback::reserve(tx, "late".to_owned(), token.clone()).await;
    assert!(matches!(
        result,
        Err(ChannelOutputDispatchError::Unavailable)
    ));
    let Some(ConnectionCommand::PreparePlayback(request)) = commands.recv().await else {
        panic!("prepare command")
    };
    assert!(request.cancellation.is_cancelled());
    assert!(request.response.is_closed());
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn attach_timeout_or_abandoned_caller_cancels_the_reservation() {
    for abandoned in [true, false] {
        let (prepared, mut commands, token) = prepared().await;
        let mut attaching = Box::pin(prepared.attach(output()));
        assert!(futures_util::poll!(attaching.as_mut()).is_pending());
        let Some(ConnectionCommand::StartPlayback { response, .. }) = commands.recv().await else {
            panic!("start command")
        };
        if abandoned {
            drop(attaching);
        } else {
            assert_eq!(
                attaching.await,
                Err(ChannelOutputDispatchError::Unavailable)
            );
        }
        assert!(response.is_closed());
        assert!(token.is_cancelled());
    }
}

#[tokio::test]
async fn cancelled_or_full_preparation_releases_its_owner() {
    for result in [
        PlaybackPreparationResult::Interrupted,
        PlaybackPreparationResult::CapacityExceeded,
    ] {
        let (tx, mut commands) = mpsc::channel(1);
        let token = CancellationToken::new();
        let expected = match result {
            PlaybackPreparationResult::Interrupted => ChannelOutputDispatchError::Cancelled,
            _ => ChannelOutputDispatchError::Unavailable,
        };
        let mut reserving = Box::pin(PreparedPlayback::reserve(
            tx,
            "output".to_owned(),
            token.clone(),
        ));
        assert!(futures_util::poll!(reserving.as_mut()).is_pending());
        let Some(ConnectionCommand::PreparePlayback(request)) = commands.recv().await else {
            panic!("prepare command")
        };
        request
            .response
            .send(result)
            .unwrap_or_else(|_| panic!("response"));
        assert!(matches!(reserving.await, Err(error) if error == expected));
        assert!(token.is_cancelled());
    }
}

#[tokio::test]
async fn synthesis_failure_or_dropped_dispatch_releases_prepared_slots() {
    let (prepared, _, token) = prepared().await;
    drop(prepared);
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn command_backpressure_releases_pcm_and_slot() {
    let (prepared, _commands, token) = prepared().await;
    prepared
        .command
        .try_send(ConnectionCommand::GatewayDisabled)
        .unwrap_or_else(|_| panic!("queue"));
    prepared
        .command
        .try_send(ConnectionCommand::GatewayDisabled)
        .unwrap_or_else(|_| panic!("queue"));
    let output = output();
    let weak = std::sync::Arc::downgrade(&output.pcm);
    assert_eq!(
        prepared.attach(output).await,
        Err(ChannelOutputDispatchError::Unavailable)
    );
    assert!(token.is_cancelled());
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn confirmed_short_playback_wins_over_its_completed_token() {
    let (prepared, mut commands, token) = prepared().await;
    let mut attaching = Box::pin(prepared.attach(output()));
    assert!(futures_util::poll!(attaching.as_mut()).is_pending());
    let Some(ConnectionCommand::StartPlayback { response, .. }) = commands.recv().await else {
        panic!("start command")
    };
    response.send(true).unwrap();
    token.cancel();
    assert_eq!(attaching.await, Ok(()));
}
