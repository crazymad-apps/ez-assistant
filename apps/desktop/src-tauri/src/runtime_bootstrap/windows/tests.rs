use super::*;
use std::{
    fs,
    io::Read as _,
    process::{Command, Stdio},
};

#[test]
#[allow(unsafe_code)]
fn native_metadata_queries_use_the_owned_process_handle() {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };
    // SAFETY: 借用当前 PID 打开新的查询句柄，成功后只由 OwnedHandle 关闭；测试不终止进程。
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            std::process::id(),
        )
    };
    assert!(!raw.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    assert!(process_owned_by_current_user(&handle).unwrap());
    assert!(process_start(&handle).unwrap() > 0);
    assert_eq!(
        crate::runtime_source::canonical_home(&process_image(&handle).unwrap()),
        crate::runtime_source::canonical_home(&std::env::current_exe().unwrap())
    );
    let process = LockProcess {
        pid: std::process::id(),
        start: process_start(&handle).unwrap(),
        image: process_image(&handle).unwrap(),
        handle,
    };
    assert!(!process.exited().unwrap());
}

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
