use super::*;

#[test]
fn pending_replacements_only_block_the_target_installation() {
    let root = tempfile::tempdir().unwrap();
    let path = install_path(root.path()).unwrap();
    verify_pending_paths(
        &path,
        &[r"\??\C:\unrelated\program.exe".into(), String::new()],
    )
    .unwrap();
    let target = format!(r"\??\{}", root.path().join("program.exe").display());
    assert!(verify_pending_paths(&path, &[target]).is_err());
}

#[test]
fn unknown_installation_is_not_adopted_and_empty_first_install_is_allowed() {
    let root = tempfile::tempdir().unwrap();
    let config = tauri::Config {
        identifier: "com.ez-assistant.f01-installation-test".into(),
        product_name: Some(format!(
            "F01-test-{}",
            root.path().file_name().unwrap().to_string_lossy()
        )),
        ..Default::default()
    };
    assert!(registered_installation(&config).unwrap().is_none());
    let path = install_path(root.path()).unwrap();
    prepare(&config, &path).unwrap();
    std::fs::write(root.path().join(IMAGES[0]), b"unknown installation").unwrap();
    assert!(prepare(&config, &path).is_err());
    assert_eq!(
        std::fs::read(root.path().join(IMAGES[0])).unwrap(),
        b"unknown installation"
    );
    assert!(install_path(Path::new("relative")).is_err());
    assert!(install_path(Path::new(r"C:\")).is_err());
}

/// 真实映像已被换掉时仍找到旧进程；只测试新建项目场景，不依赖用户 Home 或业务 HTTP。
#[test]
#[ignore = "需要显式 EZ_ASSISTANT_TEST_HOST_BINARY；只新建项目隔离 Home"]
fn installation_ends_replaced_image_without_ending_other_installation() {
    use std::process::{Child, Command, Stdio};
    struct Processes(Vec<Child>);
    impl Drop for Processes {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let binary = PathBuf::from(std::env::var_os("EZ_ASSISTANT_TEST_HOST_BINARY").unwrap())
        .canonicalize()
        .unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.runtime-test")
        .canonicalize()
        .unwrap();
    let scene = tempfile::Builder::new()
        .prefix("f01-installation-")
        .tempdir_in(root)
        .unwrap();
    let mut children = Processes(Vec::new());
    let mut images = Vec::new();
    let mut homes = Vec::new();
    for name in ["target", "unrelated"] {
        let install = scene.path().join(name);
        std::fs::create_dir(&install).unwrap();
        let image = install.join(IMAGES[1]);
        std::fs::copy(&binary, &image).unwrap();
        let home = scene.path().join(format!("{name}-custom-home"));
        std::fs::create_dir(&home).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        std::fs::write(home.join("host.toml"),format!("version = \"0.27.0\"\nmode = \"enterprise\"\n[enterprise]\ncenter_url = \"http://127.0.0.1:9\"\n[host_access]\nport = {port}\nremote_enabled = false\n")).unwrap();
        children.0.push(
            Command::new(&image)
                .arg("serve")
                .arg("--runtime-home")
                .arg(&home)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        images.push(image.canonicalize().unwrap());
        homes.push(home);
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    while homes
        .iter()
        .any(|home| !home.join("run/runtime.json").exists())
    {
        assert!(Instant::now() < deadline, "isolated Host startup failed");
        std::thread::sleep(Duration::from_millis(50));
    }
    let old_pid = children.0[0].id();
    assert!(children.0[0].try_wait().unwrap().is_none());
    std::fs::rename(&images[0], images[0].with_file_name("previous-runtime.exe")).unwrap();
    std::fs::copy(&binary, &images[0]).unwrap();
    let processes = super::super::windows::installation_processes(&images[0]).unwrap();
    assert_eq!(processes.len(), 1);
    assert_eq!(processes[0].pid(), old_pid);
    assert!(super::super::windows::verify_installation_image(&images[0]).is_err());
    processes[0].terminate_installation(&images[0]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !processes[0].exited().unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    super::super::windows::verify_installation_image(&images[0]).unwrap();
    assert!(children.0[1].try_wait().unwrap().is_none());
    assert_eq!(
        super::super::windows::installation_processes(&images[1]).unwrap()[0].pid(),
        children.0[1].id()
    );
}
