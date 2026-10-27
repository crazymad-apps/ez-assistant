use super::*;

#[test]
fn app_and_portable_layouts_follow_the_executable_after_relocation() {
    let root = Path::new("/Applications/中文 EZ Assistant.app/Contents");
    assert_eq!(
        directory_for(&root.join("MacOS/ez-assistant-runtime")),
        Some(root.join("Resources/runtime-tools/bin"))
    );
    let home = home();
    let bin = home.path().join(BIN);
    std::fs::create_dir_all(&bin).unwrap();
    assert_eq!(
        directory_for(&home.path().join("ez-assistant-runtime")),
        Some(bin)
    );
    assert!(directory_for(&home.path().join("unbundled/ez-assistant-runtime")).is_none());
}

#[test]
fn private_bin_precedes_configured_path_without_losing_other_directories() {
    let bin = Path::new("private tools/bin");
    let original = join_paths([Path::new("user/bin"), bin, Path::new("system/bin")]).unwrap();
    let path = prepend(bin, Some(&original)).unwrap();
    assert_eq!(
        split_paths(&path).collect::<Vec<_>>(),
        [bin, Path::new("user/bin"), Path::new("system/bin")]
    );
    assert_eq!(prepend(bin, None).unwrap(), bin.as_os_str());
}

#[cfg(unix)]
#[test]
fn minimal_path_shell_and_terminal_startup_find_the_private_tool() {
    use std::os::unix::fs::PermissionsExt;
    let home = home();
    let bin = home.path().join(BIN);
    std::fs::create_dir_all(&bin).unwrap();
    let rg = bin.join("rg");
    std::fs::write(&rg, "#!/bin/sh\nprintf 'bundled-rg-ok\\n'\n").unwrap();
    std::fs::set_permissions(&rg, std::fs::Permissions::from_mode(0o755)).unwrap();
    let original = OsStr::new("/usr/bin:/bin");
    let output = std::process::Command::new("/bin/sh")
        .args(["-c", "rg"])
        .env("PATH", prepend(&bin, Some(original)).unwrap())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"bundled-rg-ok\n");

    // profile 覆盖 PATH 后，现有 Bash/Zsh 启动钩子仍在当前终端恢复工具目录。
    for (shell, hook) in [
        ("/bin/bash", include_str!("../user_terminal/prompt/bash.sh")),
        ("/bin/zsh", include_str!("../user_terminal/prompt/zsh.zsh")),
    ] {
        if !Path::new(shell).exists() {
            continue;
        }
        let script = home.path().join("hook");
        std::fs::write(&script, hook).unwrap();
        let code = if shell.ends_with("zsh") {
            "PATH=/usr/bin:/bin; source \"$1\"; __ez_terminal_prompt >/dev/null; rg"
        } else {
            "PATH=/usr/bin:/bin; source \"$1\"; rg"
        };
        let output = std::process::Command::new(shell)
            .args(["-c", code, "fixture"])
            .arg(&script)
            .env("PATH", original)
            .env("EZ_ASSISTANT_TOOL_BIN", &bin)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"bundled-rg-ok\n");
    }
}

fn home() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("ez-tools-中文 路径-")
        .tempdir_in(root)
        .unwrap()
}
