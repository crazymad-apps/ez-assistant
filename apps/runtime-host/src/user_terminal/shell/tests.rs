use super::*;

#[test]
fn catalog_contains_only_resolvable_entries_and_keeps_default_identity() {
    let catalog = catalog();
    let choices: Vec<_> = catalog.iter().filter_map(|entry| entry.shell).collect();
    let agent_choices: Vec<_> = crate::resources::shell::catalog()
        .into_iter()
        .filter(|entry| entry.available)
        .map(|entry| entry.kind)
        .collect();
    assert_eq!(choices, agent_choices);

    for entry in &catalog {
        let (_, actual) = interactive(entry.shell).expect("catalog entry resolves");
        assert_eq!(entry.name, actual.name);
    }
    #[cfg(unix)]
    {
        assert!(
            catalog
                .iter()
                .any(|entry| entry.shell == Some(Shell::PosixSh))
        );
        assert!(catalog.iter().all(|entry| matches!(
            entry.shell,
            None | Some(Shell::Zsh | Shell::Bash | Shell::PosixSh)
        )));
        assert!(interactive(Some(Shell::WindowsPowershell51)).is_err());
    }
    #[cfg(windows)]
    {
        assert!(catalog.iter().any(|entry| entry.shell.is_none()));
        assert!(interactive(Some(Shell::Zsh)).is_err());
    }
}

#[cfg(unix)]
#[test]
fn default_and_explicit_shell_share_program_but_not_startup_mode() {
    let (default, actual) = prepare(None, "/bin/bash".into()).unwrap();
    let (explicit, _) = prepare(Some(Shell::Bash), "/bin/bash".into()).unwrap();
    assert_eq!(actual.shell, Some(Shell::Bash));
    assert_eq!(actual.name, "bash");
    assert_eq!(default.get_argv(), &["/bin/bash", "-l"]);
    assert_eq!(explicit.get_argv(), &["/bin/bash", "-i"]);
    assert!(!default.is_default_prog());
}

#[test]
fn invalid_default_program_never_falls_back() {
    assert!(prepare(None, PathBuf::from("relative-shell")).is_err());
    assert!(prepare(None, std::env::temp_dir()).is_err());
    assert!(prepare(None, std::env::temp_dir().join("ez-missing-shell")).is_err());
}

#[cfg(unix)]
#[test]
fn non_executable_regular_file_is_unavailable() {
    let file = tempfile::NamedTempFile::new().unwrap();
    assert!(prepare(None, file.path().to_owned()).is_err());
}

#[cfg(unix)]
#[test]
fn unknown_default_program_keeps_its_name_without_claiming_posix_sh() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("custom-login-shell");
    std::fs::write(&program, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_, actual) = prepare(None, program).unwrap();
    assert_eq!(actual.shell, None);
    assert_eq!(actual.name, "custom-login-shell");
}
