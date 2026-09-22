use super::*;

async fn command(f: &Fixture, token: &str, kind: &str, payload: Value) -> Value {
    let command = serde_json::from_value(json!({"type":kind,"payload":payload})).unwrap();
    let result: Value = f
        .command(token, command)
        .await
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    result["result"]["payload"]["payload"].clone()
}
fn skill(root: &std::path::Path, name: &str, description: &str) {
    let directory = root.join("skills").join(name);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{description}\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn same_named_skills_resolve_user_first_and_toggles_are_private() {
    let mut f = Fixture::with_domains(true, None).await;
    skill(f.home.path(), "shared", "Host fallback");
    skill(f.home.path(), "review", "Host review");
    skill(
        &f.home.path().join(format!("users/{CENTER_ID}_1")),
        "review",
        "Alice review",
    );
    skill(
        &f.home.path().join(format!("users/{CENTER_ID}_2")),
        "review",
        "Bob review",
    );
    let alice = f.login("alice").await;
    let bob = f.login("bob").await;
    for (token, expected) in [(&alice, "Alice review"), (&bob, "Bob review")] {
        let detail = command(&f, token, "get_skill_detail", json!({"name":"review"})).await;
        assert_eq!(detail["detail"]["skill"]["description"], expected);
        assert_eq!(detail["detail"]["skill"]["source"], "user_ez_assistant");
        let shared = command(&f, token, "get_skill_detail", json!({"name":"shared"})).await;
        assert_eq!(shared["detail"]["skill"]["source"], "host_supplement");
    }
    command(
        &f,
        &alice,
        "set_skill_enabled",
        json!({"name":"shared","enabled":false}),
    )
    .await;
    let a = command(&f, &alice, "get_skill_detail", json!({"name":"shared"})).await;
    let b = command(&f, &bob, "get_skill_detail", json!({"name":"shared"})).await;
    assert_eq!(a["detail"]["skill"]["enabled"], false);
    assert_eq!(b["detail"]["skill"]["enabled"], true);
    f.stop().await;
}

#[tokio::test]
async fn same_named_mcp_connections_use_private_credentials_process_directories_and_lifetimes() {
    let mut f = Fixture::with_domains(true, None).await;
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mcp_stdio_server.py");
    for id in [1, 2] {
        let root = f.home.path().join(format!("users/{CENTER_ID}_{id}"));
        // 受控 MCP 在实际启动目录记录自己的凭据标记，再运行既有协议夹具。
        let script = "import os,runpy,sys; open('owner.txt','w').write(os.environ['OWNER']); runpy.run_path(sys.argv[1],run_name='__main__')";
        std::fs::write(root.join("mcp.json"), json!({"mcpServers":{"same":{"command":"python3","args":["-u","-c",script,fixture],"env":{"OWNER":format!("user{id}")}}}}).to_string()).unwrap();
    }
    let alice = f.login("alice").await;
    let bob = f.login("bob").await;
    for token in [&alice, &bob] {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let value = command(&f, token, "get_mcp_configuration", json!({})).await;
                if value["snapshot"]["servers"][0]["runtime_state"] == "connected" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
    for id in [1, 2] {
        let root = f
            .home
            .path()
            .join(format!("users/{CENTER_ID}_{id}/mcp/same/owner.txt"));
        assert_eq!(std::fs::read_to_string(root).unwrap(), format!("user{id}"));
    }
    f.request(reqwest::Method::POST, "/auth/logout", Some(&alice))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let b = command(&f, &bob, "get_mcp_configuration", json!({})).await;
    assert_eq!(b["snapshot"]["servers"][0]["runtime_state"], "connected");
    assert!(!b.to_string().contains("user1"));
    f.stop().await;
}

#[tokio::test]
async fn gateway_identity_and_pairing_windows_are_owned_by_each_user() {
    let mut f = Fixture::with_domains(true, None).await;
    let alice = f.login("alice").await;
    let bob = f.login("bob").await;
    let a = f.services(&alice).await;
    let b = f.services(&bob).await;
    a.device_gateway.set_enabled(true).await.unwrap();
    a.device_gateway.open_pairing_window().await.unwrap();
    let a_snapshot = a.device_gateway.snapshot().await.unwrap();
    assert!(a_snapshot.enabled);
    assert!(!b.device_gateway.snapshot().await.unwrap().enabled);
    b.device_gateway.set_enabled(true).await.unwrap();
    let b_snapshot = b.device_gateway.snapshot().await.unwrap();
    assert_ne!(a_snapshot.installation_id, b_snapshot.installation_id);
    assert_ne!(
        a_snapshot.certificate_fingerprint,
        b_snapshot.certificate_fingerprint
    );
    assert!(b_snapshot.pairing_window.is_none());
    f.request(reqwest::Method::POST, "/auth/logout", Some(&alice))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert!(b.device_gateway.snapshot().await.unwrap().enabled);
    f.stop().await;
}
