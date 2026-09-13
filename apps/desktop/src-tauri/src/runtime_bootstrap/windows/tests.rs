use super::*;
use std::{
    fs,
    io::Read as _,
    process::{Command, Stdio},
};

#[test]
fn private_discovery_is_read_without_modification_and_lock_contention_is_observed() {
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join("run");
    fs::create_dir(&run).unwrap();
    let path = run.join("runtime.json");
    fs::write(&path, b"fixture").unwrap();
    let mut file = open_private_file(root.path(), &path, false).unwrap();
    let mut content = String::new();
    file.read_to_string(&mut content).unwrap();
    assert_eq!(content, "fixture");
    let lock_path = run.join("runtime.lock");
    fs::write(&lock_path, b"").unwrap();
    let lock = open_private_file(root.path(), &lock_path, true).unwrap();
    lock.try_lock().unwrap();
    let other = open_private_file(root.path(), &lock_path, true).unwrap();
    assert!(matches!(
        other.try_lock(),
        Err(fs::TryLockError::WouldBlock)
    ));
    drop(lock);
    other.try_lock().unwrap();
}

#[test]
fn private_discovery_rejects_junction_parent_and_non_user_owner() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("runtime.json"), b"fixture").unwrap();
    let junction = root.path().join("run");
    let mut command = Command::new("cmd.exe");
    command
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&target)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    assert!(command.status().unwrap().success());
    assert!(open_private_file(root.path(), &junction.join("runtime.json"), false).is_err());
    fs::remove_dir(junction).unwrap();
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
    let system_file = File::open(system.join("System32/cmd.exe")).unwrap();
    assert!(!owned_by_current_user(&system_file).unwrap());
    assert!(open_private_file(root.path(), root.path(), false).is_err());
    assert!(open_private_file(root.path(), &root.path().join("missing"), false).is_err());
}
