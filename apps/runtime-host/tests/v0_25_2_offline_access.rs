//! Local management IPC only: these fixtures never open a Store or start a business Runtime.
use serde_json::{Value, json};
use std::{
    fs::{self, File, Permissions},
    io::Write,
    net::TcpListener,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn invoke(home: &Path, operation: &str, input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ez-assistant-runtime"))
        .args(["access", operation, "--runtime-home"])
        .arg(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}
fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn failure(output: Output, code: &str) {
    assert!(!output.status.success());
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["error"]["code"], code);
}
fn private_home(home: &Path) {
    fs::create_dir(home).unwrap();
    fs::set_permissions(home, Permissions::from_mode(0o700)).unwrap();
}
fn configure(home: &Path, revision: Value, port: u16, remote: bool) -> Output {
    invoke(
        home,
        "configure",
        json!({"expected_revision":revision,"configuration":{
            "port":port,"scheme":"http","remote_enabled":remote,"server_names":[]
        }})
        .to_string()
        .as_bytes(),
    )
}
fn assert_no_runtime(home: &Path) {
    assert!(!home.join("data").exists());
    assert!(!home.join("run/runtime.json").exists());
    let names: Vec<_> = fs::read_dir(home)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        names
            .iter()
            .all(|name| name == "run" || name == "config.toml"),
        "{names:?}"
    );
}

#[test]
fn metadata_and_missing_read_do_not_create_home() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("missing");
    let output = Command::new(env!("CARGO_BIN_EXE_ez-assistant-runtime"))
        .arg("--build-info-json")
        .env("EZ_ASSISTANT_RUNTIME_HOME", &home)
        .output()
        .unwrap();
    assert_eq!(
        success(output),
        json!({"version":"0.25.2","min_compatible_version":"0.25.2"})
    );
    let read = success(invoke(&home, "read", b""));
    assert_eq!(read["revision"], Value::Null);
    assert_eq!(read["password_configured"], false);
    assert_eq!(read["configuration"]["remote_enabled"], false);
    assert!(!home.exists());
}

#[test]
fn read_never_repairs_permissions_or_creates_lock() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    private_home(&home);
    let config = home.join("config.toml");
    fs::write(&config, "schema_version = 1\n").unwrap();
    fs::set_permissions(&config, Permissions::from_mode(0o644)).unwrap();
    failure(invoke(&home, "read", b""), "configuration_unavailable");
    assert_eq!(
        fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o644
    );
    fs::set_permissions(&config, Permissions::from_mode(0o400)).unwrap();
    success(invoke(&home, "read", b""));
    assert_eq!(
        fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o400
    );
    assert!(!home.join("run").exists());
}

#[test]
fn password_and_configuration_share_cas_and_preserve_other_sections() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    private_home(&home);
    fs::write(
        home.join("config.toml"),
        "# preserve this comment\nschema_version = 1\n[custom]\nvalue = 23\n",
    )
    .unwrap();
    fs::set_permissions(home.join("config.toml"), Permissions::from_mode(0o600)).unwrap();
    let revision = success(invoke(&home, "read", b""))["revision"].clone();
    let saved = success(invoke(
        &home,
        "set-password",
        json!({"expected_revision":revision,"password":"offline-password-123"})
            .to_string()
            .as_bytes(),
    ));
    assert_eq!(saved["password_configured"], true);
    assert_eq!(saved["effective_on_next_start"], true);
    assert!(!saved.to_string().contains("offline-password-123"));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let before = fs::read(home.join("config.toml")).unwrap();
    failure(
        configure(&home, revision, port, true),
        "configuration_conflict",
    );
    failure(
        configure(&home, saved["revision"].clone(), port, true),
        "port_in_use",
    );
    assert_eq!(fs::read(home.join("config.toml")).unwrap(), before);
    drop(listener);
    let configured = success(configure(&home, saved["revision"].clone(), port, true));
    assert_eq!(configured["configuration"]["port"], port);
    assert_eq!(configured["configuration"]["remote_enabled"], true);
    let text = fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(text.contains("# preserve this comment") && text.contains("value = 23"));
    assert!(text.contains("$argon2id$"));
    assert!(!text.contains("offline-password-123"));
    assert_no_runtime(&home);
}

#[test]
fn canonical_alias_shares_instance_lock_and_revision() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let alias = temp.path().join("alias");
    private_home(&home);
    symlink(&home, &alias).unwrap();
    fs::create_dir(home.join("run")).unwrap();
    let lock = File::create(home.join("run/runtime.lock")).unwrap();
    lock.set_permissions(Permissions::from_mode(0o600)).unwrap();
    lock.try_lock().unwrap();
    failure(
        invoke(
            &alias,
            "set-password",
            br#"{"password":"offline-password-123"}"#,
        ),
        "busy",
    );
    assert!(!home.join("config.toml").exists());
    drop(lock);
    let saved = success(invoke(
        &alias,
        "set-password",
        br#"{"password":"offline-password-123"}"#,
    ));
    assert_eq!(
        success(invoke(&home, "read", b""))["revision"],
        saved["revision"]
    );
    assert_no_runtime(&home);
}

#[test]
fn invalid_requests_cannot_create_home_or_echo_secrets() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    for input in [
        b"not-json-secret".to_vec(),
        vec![b'x'; 65537],
        br#"{"password":"secret","unexpected":true}"#.to_vec(),
    ] {
        let output = invoke(&home, "set-password", &input);
        assert!(!String::from_utf8_lossy(&output.stdout).contains("secret"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("secret"));
        failure(output, "invalid_request");
        assert!(!home.exists());
    }
}

#[test]
fn remote_without_password_and_invalid_tls_do_not_save_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    failure(configure(&home, Value::Null, 7240, true), "invalid_request");
    assert!(!home.join("config.toml").exists());
    failure(invoke(&home,"configure", json!({"configuration":{
        "port":7240,"scheme":"https","remote_enabled":false,"server_names":[],
        "tls_certificate":home.join("missing.pem"),"tls_private_key":home.join("missing.key")
    }}).to_string().as_bytes()),"invalid_request");
    assert!(!home.join("config.toml").exists());
    assert_no_runtime(&home);
}

#[test]
fn detached_child_creates_session_before_any_business_initialization() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ez-assistant-runtime"))
        .args(["serve", "--detached", "--password-stdin", "--runtime-home"])
        .arg(&home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = nix::unistd::Pid::from_raw(child.id().try_into().unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !home.join("run/runtime.lock").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let session = nix::unistd::getsid(Some(pid));
    let group = nix::unistd::getpgid(Some(pid));
    // Invalid UTF-8 fails before access preparation, discovery publication or Store initialization.
    child.stdin.take().unwrap().write_all(&[0xff]).unwrap();
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    assert!(!child.wait().unwrap().success());
    assert_eq!(session.unwrap(), pid);
    assert_eq!(group.unwrap(), pid);
    assert_no_runtime(&home);
}

#[test]
fn startup_signals_do_not_hang_on_unfinished_password_input() {
    for signal in [
        nix::sys::signal::Signal::SIGINT,
        nix::sys::signal::Signal::SIGTERM,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let mut child = Command::new(env!("CARGO_BIN_EXE_ez-assistant-runtime"))
            .args(["serve", "--password-stdin", "--runtime-home"])
            .arg(&home)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !home.join("run/runtime.lock").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(child.id().try_into().unwrap()),
            signal,
        )
        .unwrap();
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let exited = child.try_wait().unwrap().is_some();
        if !exited {
            child.kill().unwrap();
        }
        assert!(!child.wait().unwrap().success());
        assert!(exited, "signal must not wait for EOF on stdin");
        assert_no_runtime(&home);
    }
}

#[test]
fn probe_reads_existing_kernel_lock_without_creating_or_chmodding_files() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    assert_eq!(success(invoke(&home, "probe", b""))["lock_held"], false);
    assert!(!home.exists());
    private_home(&home);
    fs::create_dir(home.join("run")).unwrap();
    fs::set_permissions(home.join("run"), Permissions::from_mode(0o700)).unwrap();
    let path = home.join("run/runtime.lock");
    let lock = File::create(&path).unwrap();
    lock.set_permissions(Permissions::from_mode(0o600)).unwrap();
    lock.try_lock().unwrap();
    assert_eq!(success(invoke(&home, "probe", b""))["lock_held"], true);
    drop(lock);
    assert_eq!(success(invoke(&home, "probe", b""))["lock_held"], false);
    fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();
    failure(invoke(&home, "probe", b""), "configuration_unavailable");
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert_no_runtime(&home);
}
