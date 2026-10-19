use super::input::Input;
use assistant_protocol::DesktopInput;

#[test]
fn inputs_reject_gaps_replay_and_invalid_coordinates() {
    let mut input = Input::new();
    assert!(
        input
            .prepare(1, &DesktopInput::Move { x: 0.5, y: 0.0 })
            .is_ok()
    );
    assert!(input.prepare(1, &DesktopInput::Heartbeat).is_err());
    assert!(input.prepare(3, &DesktopInput::Heartbeat).is_err());
    assert!(
        input
            .prepare(
                2,
                &DesktopInput::Move {
                    x: f64::NAN,
                    y: 1.0
                }
            )
            .is_err()
    );
    assert!(
        Input::new()
            .prepare(1, &DesktopInput::Scroll { x: i32::MIN, y: 0 })
            .is_err()
    );
}
#[test]
fn pressed_keys_are_owned_and_release_cancels_text() {
    let mut input = Input::new();
    let key = |down, repeat| DesktopInput::Key {
        code: "ControlLeft".into(),
        down,
        repeat,
    };
    assert!(input.prepare(1, &key(true, false)).is_ok());
    assert!(input.prepare(2, &key(true, true)).is_ok());
    assert_eq!(input.pressed.keys.len(), 1);
    assert!(input.prepare(3, &DesktopInput::TextStart).is_err());
    assert!(input.prepare(4, &DesktopInput::Release).is_ok());
    assert!(input.pressed.keys.is_empty());
    assert!(input.prepare(5, &DesktopInput::TextStart).is_ok());
    assert!(
        input
            .prepare(
                6,
                &DesktopInput::TextChunk {
                    text: "中文🙂".into()
                }
            )
            .is_ok()
    );
    assert_eq!(
        input.prepare(7, &DesktopInput::TextEnd).unwrap(),
        Some("text 7 e4b8ade69687f09f9982".into())
    );
    assert!(input.prepare(8, &DesktopInput::TextEnd).is_err());
}
#[test]
fn text_is_bounded_and_never_interleaves_physical_input() {
    let mut input = Input::new();
    input.prepare(1, &DesktopInput::TextStart).unwrap();
    assert!(
        input
            .prepare(
                2,
                &DesktopInput::Key {
                    code: "KeyA".into(),
                    down: true,
                    repeat: false
                }
            )
            .is_err()
    );
    assert!(
        input
            .prepare(
                2,
                &DesktopInput::TextChunk {
                    text: "a".repeat(16_385)
                }
            )
            .is_err()
    );
    assert!(input.pressed.keys.is_empty());
}
#[test]
fn whole_device_control_slot_is_exclusive_until_owner_drops() {
    let controls = super::Controls::default();
    let first = controls.exclusive.clone().try_acquire_owned().unwrap();
    assert!(controls.exclusive.clone().try_acquire_owned().is_err());
    drop(first);
    assert!(controls.exclusive.clone().try_acquire_owned().is_ok());
}
#[cfg(unix)]
#[tokio::test]
async fn native_timeout_and_eof_are_not_success() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input-helper");
    std::fs::write(&path, "#!/bin/sh\nprintf 'ready\\n'\nread line\nprintf 'wrong-ack\\n'\nwhile read line; do exit 0; done\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut input = super::native::NativeInput::start_helper(path, "test")
        .await
        .unwrap();
    assert!(input.command(1, &DesktopInput::Heartbeat).await.is_err());
    input.stop().await;
}
