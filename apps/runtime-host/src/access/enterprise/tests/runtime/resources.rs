use super::*;
use reqwest::Method;

async fn session(f: &Fixture, token: &str) -> String {
    let created: Value = f
        .command(token, RuntimeCommand::CreateSession(Default::default()))
        .await
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    created["result"]["payload"]["payload"]["session"]["session_id"]
        .as_str()
        .unwrap()
        .into()
}

#[tokio::test]
async fn files_uploads_and_native_paths_stay_with_the_authenticated_user() {
    let mut f = Fixture::with_domains(true, None).await;
    let alice = f.login("alice").await;
    let bob = f.login("bob").await;
    let sid = session(&f, &alice).await;
    let a = f.services(&alice).await;
    let b = f.services(&bob).await;
    let own = a.paths.user_root.join("own.txt");
    std::fs::write(&own, "Alice private content").unwrap();
    for route in ["preview", "download"] {
        let endpoint = format!("/host-files/{route}");
        assert_eq!(
            f.request(Method::POST, &endpoint, Some(&alice))
                .json(&json!({"path": own}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert!(
            !f.request(Method::POST, &endpoint, Some(&bob))
                .json(&json!({"path": own}))
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
    }
    for path in [
        a.paths.user_root.clone(),
        f.home.path().to_owned(),
        f.home.path().join("run"),
    ] {
        assert!(
            !f.request(Method::POST, "/host-files/select-directory", Some(&bob))
                .json(&json!({"path": path}))
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
    }
    #[cfg(unix)]
    {
        let alias = b.paths.user_root.join("alias.txt");
        std::os::unix::fs::symlink(&own, &alias).unwrap();
        assert!(
            !f.request(Method::POST, "/host-files/download", Some(&bob))
                .json(&json!({"path": alias}))
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
        let external = tempfile::tempdir().unwrap();
        std::fs::write(external.path().join("file.txt"), "public").unwrap();
        let hidden_alias = a.paths.user_root.join("outside");
        std::os::unix::fs::symlink(external.path(), &hidden_alias).unwrap();
        assert!(
            !f.request(Method::POST, "/host-files/download", Some(&bob))
                .json(&json!({"path":hidden_alias.join("file.txt")}))
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
        let listing: Value = f
            .request(Method::POST, "/host-files/list", Some(&bob))
            .json(&json!({"path":b.paths.user_root,"include_hidden":true}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(!listing.to_string().contains("alias.txt"));
    }
    let form = || {
        reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::text("private upload").file_name("same.txt"),
        )
    };
    let upload: Value = f
        .request(
            Method::POST,
            &format!("/sessions/{sid}/attachments"),
            Some(&alice),
        )
        .multipart(form())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = upload["attachment"]["attachment_id"].as_str().unwrap();
    let route = format!("/sessions/{sid}/attachments/{id}/download");
    assert_eq!(
        f.request(Method::GET, &route, Some(&alice))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "private upload"
    );
    assert_eq!(
        f.request(Method::GET, &route, Some(&bob))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.request(
            Method::POST,
            &format!("/sessions/{sid}/attachments"),
            Some(&bob)
        )
        .multipart(form())
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NOT_FOUND
    );
    let native = format!("/sessions/{sid}/resource-files/native-path");
    let locator = json!({"root":{"type":"session_private"},"relative_path":""});
    assert_eq!(
        f.request(Method::POST, &native, Some(&alice))
            .json(&locator)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.request(Method::POST, &native, Some(&alice))
            .header("x-ez-host-bootstrap", "bootstrap-secret")
            .json(&locator)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        f.request(Method::POST, &native, Some(&bob))
            .header("x-ez-host-bootstrap", "bootstrap-secret")
            .json(&locator)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        std::fs::read_dir(a.paths.user_root.join("data/staging/uploads"))
            .unwrap()
            .count(),
        0
    );
    assert!(!f.home.path().join("data/staging/uploads").exists());
    f.stop().await;
}

#[tokio::test]
async fn replaced_cookie_context_cannot_read_resources_or_logout_the_new_user() {
    let mut f = Fixture::with_domains(true, None).await;
    let alice = f.login("alice").await;
    let bob = f.login("bob").await;
    let a = f.permit(&alice).login_context().unwrap();
    let b = f.permit(&bob).login_context().unwrap();
    let cookie = format!(
        "ez_host_session_http_{}={bob}",
        f.url.rsplit(':').next().unwrap()
    );
    for route in ["/auth/logout", "/host-files/list"] {
        let response = f
            .request(Method::POST, route, None)
            .header("Cookie", &cookie)
            .header("Origin", &f.url)
            .header("x-ez-login-context", &a)
            .json(&json!({"path":f.home.path(),"include_hidden":true}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "login_context_changed"
        );
    }
    let base = "/sessions/missing/attachments/missing/download";
    for (context, status) in [(&a, StatusCode::CONFLICT), (&b, StatusCode::NOT_FOUND)] {
        assert_eq!(
            f.request(
                Method::GET,
                &format!("{base}?login_context={context}"),
                None
            )
            .header("Cookie", &cookie)
            .header("Origin", &f.url)
            .send()
            .await
            .unwrap()
            .status(),
            status
        );
    }
    assert_eq!(f.session(&bob).await.status(), StatusCode::OK);
    f.stop().await;
}

#[cfg(unix)]
type Socket = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

#[cfg(unix)]
async fn terminal(f: &Fixture, token: &str, sid: &str) -> (Socket, Value) {
    let url = format!("{}/user-terminals/socket", f.url.replace("http:", "ws:"));
    let open = json!({"type":"open", "client_compatibility":ClientCompatibility::current(),"bearer":token,
        "source":{"type":"session","session_id":sid,"locator":{"root":{"type":"session_private"},"relative_path":""}},
        "size":{"cols":80,"rows":24},"shell":"posix_sh"});
    tokio::task::spawn_blocking(move || {
        let (mut socket, _) = tungstenite::connect(url).unwrap();
        if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_ref() {
            stream
                .set_read_timeout(Some(Duration::from_secs(8)))
                .unwrap();
        }
        socket
            .send(tungstenite::Message::Text(open.to_string().into()))
            .unwrap();
        let value = terminal_notice(&mut socket);
        (socket, value)
    })
    .await
    .unwrap()
}
#[cfg(unix)]
fn terminal_notice(socket: &mut Socket) -> Value {
    loop {
        match socket.read().unwrap() {
            tungstenite::Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            tungstenite::Message::Binary(_) => socket
                .send(tungstenite::Message::Text("{\"type\":\"ack\"}".into()))
                .unwrap(),
            tungstenite::Message::Ping(_) => socket.flush().unwrap(),
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_resolves_its_users_session_and_logout_does_not_close_other_users_pty() {
    let mut f = Fixture::with_domains(true, None).await;
    let alice = f.login("alice").await;
    let bob = f.login("bob").await;
    let a = session(&f, &alice).await;
    let b = session(&f, &bob).await;
    let (_, denied) = terminal(&f, &bob, &a).await;
    assert_eq!(denied["type"], "error");
    let (mut a_socket, a_notice) = terminal(&f, &alice, &a).await;
    let (mut b_socket, b_notice) = terminal(&f, &bob, &b).await;
    assert_eq!(a_notice["type"], "created", "{a_notice}");
    assert_eq!(b_notice["type"], "created", "{b_notice}");
    assert_eq!(
        f.request(Method::POST, "/auth/logout", Some(&alice))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    tokio::task::spawn_blocking(move || {
        assert_eq!(terminal_notice(&mut a_socket)["type"], "closed");
        b_socket
            .send(tungstenite::Message::Text("{\"type\":\"close\"}".into()))
            .unwrap();
        assert_eq!(terminal_notice(&mut b_socket)["type"], "closed");
    })
    .await
    .unwrap();
    assert_eq!(f.session(&bob).await.status(), StatusCode::OK);
    f.stop().await;
}

#[tokio::test]
async fn logout_interrupts_an_incomplete_upload_and_removes_only_its_staging_file() {
    use futures_util::StreamExt as _;
    let mut f = Fixture::with_domains(true, None).await;
    let alice = f.login("alice").await;
    let sid = session(&f, &alice).await;
    let services = f.services(&alice).await;
    let staging = services.paths.user_root.join("data/staging/uploads");
    let release = CancellationToken::new();
    let held = release.clone();
    let stream = futures_util::stream::once(async {
        Ok::<_, std::io::Error>("--fixture\r\nContent-Disposition: form-data; name=\"file\"; filename=\"partial.txt\"\r\nContent-Type: text/plain\r\n\r\npartial bytes\r\n")
    }).chain(futures_util::stream::once(async move { held.cancelled().await; Ok::<_, std::io::Error>("--fixture--\r\n") }));
    let request = f
        .request(
            Method::POST,
            &format!("/sessions/{sid}/attachments"),
            Some(&alice),
        )
        .header("Content-Type", "multipart/form-data; boundary=fixture")
        .body(reqwest::Body::wrap_stream(stream));
    let upload = tokio::spawn(async move { request.send().await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while std::fs::read_dir(&staging).unwrap().count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    f.request(Method::POST, "/auth/logout", Some(&alice))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while std::fs::read_dir(&staging).unwrap().count() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    release.cancel();
    let result = tokio::time::timeout(Duration::from_secs(3), upload)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err() || !result.unwrap().status().is_success());
    f.stop().await;
}
