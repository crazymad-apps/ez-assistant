use std::{
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn verify_v0_2_succeeds_without_using_provider_configuration() {
    let output = Command::new(env!("CARGO_BIN_EXE_runtime-harness"))
        .args(["verify", "v0.2"])
        .env("DEEPSEEK_API_KEY", "sentinel-do-not-use")
        .env("DEEPSEEK_BASE_URL", "http://127.0.0.1:9")
        .output()
        .expect("runtime-harness binary must start");

    assert!(
        output.status.success(),
        "verify failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("verify output must be UTF-8");
    assert!(stdout.contains("verify v0.2 (6 scenarios)"));
    assert!(stdout.contains("summary: 6 passed, 0 failed"));
    assert!(!stdout.contains("sentinel-do-not-use"));
}

#[test]
fn verify_v0_3_is_cumulative_and_offline() {
    let output = Command::new(env!("CARGO_BIN_EXE_runtime-harness"))
        .args(["verify", "v0.3"])
        .env("DEEPSEEK_API_KEY", "sentinel-do-not-use")
        .env("DEEPSEEK_BASE_URL", "http://127.0.0.1:9")
        .output()
        .expect("runtime-harness binary must start");

    assert!(
        output.status.success(),
        "verify failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("verify output must be UTF-8");
    assert!(stdout.contains("verify v0.3 (14 scenarios)"));
    assert!(stdout.contains("[PASS] plain_text"));
    assert!(stdout.contains("[PASS] context_failure_boundaries"));
    assert!(stdout.contains("summary: 14 passed, 0 failed"));
    assert!(!stdout.contains("sentinel-do-not-use"));
}

#[test]
fn chat_without_a_key_fails_fast_without_leaking_provider_values() {
    let output = Command::new(env!("CARGO_BIN_EXE_runtime-harness"))
        .arg("chat")
        .current_dir(std::env::temp_dir())
        .env_remove("DEEPSEEK_API_KEY")
        .env("DEEPSEEK_BASE_URL", "http://provider-secret.invalid")
        .env("DEEPSEEK_MODEL", "model-secret")
        .output()
        .expect("runtime-harness binary must start");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("chat error must be UTF-8");
    assert!(stderr.contains("missing DEEPSEEK_API_KEY"));
    assert!(!stderr.contains("provider-secret"));
    assert!(!stderr.contains("model-secret"));
}

#[test]
fn chat_idle_controls_run_without_contacting_the_provider() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_runtime-harness"))
        .arg("chat")
        .current_dir(std::env::temp_dir())
        .env("DEEPSEEK_API_KEY", "sentinel-do-not-use")
        .env("DEEPSEEK_BASE_URL", "http://127.0.0.1:9")
        .env("DEEPSEEK_MODEL", "offline-no-request")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("runtime-harness binary must start");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(b"/state\n/compact\n/cancel\n/reset\n/state\n/quit\n")
        .expect("write commands");
    let output = child.wait_with_output().expect("chat must exit");

    assert!(
        output.status.success(),
        "chat failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("chat output must be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("chat error output must be UTF-8");
    assert_eq!(stdout.matches("run: none").count(), 2);
    assert!(stdout.contains("session journal reset"));
    assert!(stdout.contains("/compact: no_op"));
    assert!(stderr.contains("there is no active run to cancel"));
    assert!(!stdout.contains("sentinel-do-not-use"));
}
