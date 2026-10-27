use super::*;
use std::{
    fs::OpenOptions,
    process::{Child, Stdio},
    time::Duration,
};

fn scene() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("m4-install-native-")
        .tempdir_in(root)
        .unwrap()
}

fn fixture(target: &Path, name: &str, home: &Path) -> Child {
    let executable = image(target, name);
    fs::create_dir_all(executable.parent().unwrap()).unwrap();
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    fs::create_dir_all(home.join("run")).unwrap();
    for path in [home.to_owned(), home.join("run")] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let child = Command::new(executable)
        .args([
            "--ignored",
            "--exact",
            "runtime_bootstrap::installation_macos::tests::installation_fixture",
            "--nocapture",
        ])
        .env("EZ_M4_FIXTURE_HOME", home)
        .env("EZ_M4_FIXTURE_ROLE", name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !home.join("ready").exists() {
        assert!(Instant::now() < deadline, "fixture did not become ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    child
}

#[test]
#[ignore = "只由隔离安装测试作为子进程执行，不读取正式 Home"]
fn installation_fixture() {
    let home =
        PathBuf::from(std::env::var_os("EZ_M4_FIXTURE_HOME").expect("isolated fixture home"));
    assert!(
        home.canonicalize()
            .unwrap()
            .components()
            .any(|part| part.as_os_str() == ".runtime-test")
    );
    let held = if std::env::var("EZ_M4_FIXTURE_ROLE").unwrap() == HOST {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(home.join("run/runtime.lock"))
            .unwrap();
        file.try_lock().unwrap();
        Some(file)
    } else {
        None
    };
    fs::write(home.join("ready"), b"ready").unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        std::hint::black_box(&held);
    }
}

#[test]
fn stops_only_this_installation_and_releases_the_actual_custom_home_lock() {
    let scene = scene();
    let root = scene.path().canonicalize().unwrap();
    let target = root.join("Test.app");
    let mut desktop = fixture(&target, DESKTOP, &root.join("desktop-home"));
    let home = root.join("custom-runtime-home");
    let mut host = fixture(&target, HOST, &home);
    let other = root.join("Other.app");
    let mut control = fixture(&other, HOST, &root.join("other-home"));
    let result = prepare(&target, geteuid().as_raw());
    assert!(desktop.try_wait().unwrap().is_some());
    assert!(host.try_wait().unwrap().is_some());
    assert!(control.try_wait().unwrap().is_none());
    control.kill().unwrap();
    control.wait().unwrap();
    result.unwrap();
    native_stop::open_lock(&home)
        .unwrap()
        .unwrap()
        .try_lock()
        .unwrap();
    assert_eq!(fs::read(home.join("ready")).unwrap(), b"ready");
}

#[test]
fn owner_mismatch_refuses_before_ending_any_process() {
    let scene = scene();
    let root = scene.path().canonicalize().unwrap();
    let target = root.join("Test.app");
    let mut desktop = fixture(&target, DESKTOP, &root.join("desktop-home"));
    let result = prepare(&target, geteuid().as_raw() + 1);
    assert!(desktop.try_wait().unwrap().is_none());
    desktop.kill().unwrap();
    desktop.wait().unwrap();
    assert!(result.is_err());
}

#[test]
fn fixed_product_location_and_bundle_identity_are_required() {
    let config = tauri::Config {
        identifier: "com.example.install-test".into(),
        product_name: Some("EZ Installation Test".into()),
        ..Default::default()
    };
    assert!(installation_path(&config, Path::new("/tmp/EZ Installation Test.app")).is_err());
    assert!(
        installation_path(
            &config,
            Path::new("/Applications/../tmp/EZ Installation Test.app")
        )
        .is_err()
    );
    let scene = scene();
    fs::create_dir_all(scene.path().join("Contents")).unwrap();
    let info = plist::Value::Dictionary(plist::Dictionary::from_iter([(
        "CFBundleIdentifier",
        plist::Value::String("com.example.other".into()),
    )]));
    info.to_file_xml(scene.path().join("Contents/Info.plist"))
        .unwrap();
    assert!(validate_bundle(scene.path(), &config.identifier).is_err());
}
