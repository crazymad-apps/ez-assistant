//! 网络初始化与本机登录共用状态，覆盖真实 HTTP 核验中的迟到结果。
use super::*;
fn key() -> UserKey {
    UserKey {
        center_id: CENTER_ID.into(),
        user_id: 1,
    }
}
fn candidate(mock: &Mock) -> Value {
    let serial = mock.serial.fetch_add(1, Ordering::SeqCst) + 1;
    let token = format!("ct_{serial:064x}");
    mock.tokens.lock().unwrap().insert(token.clone(), 1);
    let mut value = identity(1, false);
    value["token"] = json!(token);
    value["llm_key"] = json!(format!("cl_{serial:064x}"));
    value
}
#[tokio::test]
async fn network_reuses_current_login_and_scope_close_preserves_local_access() {
    let f = Fixture::new().await;
    let token = f.login("alice").await;
    let credentials = &f.state.access.credentials;
    let (before, _) = credentials.current_center_login(&key()).unwrap();
    let initialized = credentials
        .network_user(
            &f.state.access.center().unwrap(),
            key(),
            CancellationToken::new(),
            || async { panic!("must reuse current login") },
        )
        .await
        .unwrap();
    let permit = initialized.commit().unwrap();
    assert!(!permit.native);
    permit.logout();
    assert!(Arc::ptr_eq(
        &before,
        &credentials.current_center_login(&key()).unwrap().0
    ));
    assert_eq!(f.session(&token).await.status(), StatusCode::OK);
    assert!(
        credentials
            .state
            .lock()
            .unwrap()
            .network_initializations
            .is_empty()
    );
}
#[tokio::test]
async fn network_concurrent_first_access_issues_once_and_cancelled_waiter_does_not_leak_gate() {
    let f = Fixture::new().await;
    let credentials = &f.state.access.credentials;
    let client = f.state.access.center().unwrap();
    let issued = AtomicUsize::new(0);
    let open = || async {
        credentials
            .network_user(&client, key(), CancellationToken::new(), || async {
                issued.fetch_add(1, Ordering::SeqCst);
                Ok(candidate(&f.mock))
            })
            .await
            .unwrap()
            .commit()
            .unwrap()
    };
    let (a, b) = tokio::join!(open(), open());
    assert_eq!(issued.load(Ordering::SeqCst), 1);
    a.logout();
    assert!(b.check().is_ok());
    assert!(
        credentials
            .state
            .lock()
            .unwrap()
            .network_initializations
            .is_empty()
    );
    let held = credentials
        .network_user(&client, key(), CancellationToken::new(), || async {
            panic!("reuse")
        })
        .await
        .unwrap();
    let stop = CancellationToken::new();
    let second = credentials.network_user(&client, key(), stop.clone(), || async {
        panic!("cancelled waiter")
    });
    tokio::pin!(second);
    tokio::select! { _ = &mut second => panic!("gate must be held"), _ = tokio::task::yield_now() => {} }
    stop.cancel();
    assert!(second.await.is_err());
    drop(held);
    assert!(
        credentials
            .state
            .lock()
            .unwrap()
            .network_initializations
            .is_empty()
    );
    b.logout();
}
#[tokio::test]
async fn network_late_initialization_cannot_replace_explicit_login() {
    let f = Fixture::new().await;
    let credentials = f.state.access.credentials.clone();
    let client = f.state.access.center().unwrap();
    let held = Arc::new(Held::default());
    *f.mock.hold.lock().unwrap() = Some(held.clone());
    let value = candidate(&f.mock);
    let init = tokio::spawn(async move {
        credentials
            .network_user(&client, key(), CancellationToken::new(), || async {
                Ok(value)
            })
            .await?
            .commit()
    });
    held.started.notified().await;
    let local = f.login("alice").await;
    held.release.notify_one();
    assert!(init.await.unwrap().is_err());
    assert_eq!(f.session(&local).await.status(), StatusCode::OK);
    assert!(
        f.state
            .access
            .credentials
            .state
            .lock()
            .unwrap()
            .network_initializations
            .is_empty()
    );
}
#[tokio::test]
async fn network_cancelled_uncommitted_login_rolls_back_only_its_candidate() {
    let f = Fixture::new().await;
    let credentials = &f.state.access.credentials;
    let client = f.state.access.center().unwrap();
    let stop = CancellationToken::new();
    let pending = credentials
        .network_user(&client, key(), stop.clone(), || async {
            Ok(candidate(&f.mock))
        })
        .await
        .unwrap();
    stop.cancel();
    assert!(pending.commit().is_err());
    assert!(credentials.current_center_login(&key()).is_err());
    let state = credentials.state.lock().unwrap();
    assert!(state.network_initializations.is_empty());
    assert!(state.sessions.values().all(|session| !session.valid()));
}

#[tokio::test]
async fn network_pending_reuse_cannot_restore_user_after_target_logout() {
    let f = Fixture::new().await;
    let token = f.login("alice").await;
    let credentials = f.state.access.credentials.clone();
    let client = f.state.access.center().unwrap();
    let held = Arc::new(Held::default());
    *f.mock.hold.lock().unwrap() = Some(held.clone());
    let init = tokio::spawn(async move {
        credentials
            .network_user(&client, key(), CancellationToken::new(), || async {
                panic!("reuse")
            })
            .await?
            .commit()
    });
    held.started.notified().await;
    assert!(
        f.state
            .access
            .credentials
            .enterprise_logout(&f.permit(&token))
            .await
    );
    held.release.notify_one();
    assert!(init.await.unwrap().is_err());
    assert!(
        f.state
            .access
            .credentials
            .current_center_login(&key())
            .is_err()
    );
    assert!(
        f.state
            .access
            .credentials
            .state
            .lock()
            .unwrap()
            .network_initializations
            .is_empty()
    );
}
