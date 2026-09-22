use super::*;

fn fixture() -> (tempfile::TempDir, UserPaths) {
    let dir = tempfile::tempdir().unwrap();
    let host = dir.path().join("host[private]");
    for name in ["users/alice", "users/bob", "skills", "run", "backups"] {
        std::fs::create_dir_all(host.join(name)).unwrap();
    }
    let paths = UserPaths::new(&host.join("users/alice"), vec![dir.path().join("tls.key")]);
    (dir, paths)
}

#[test]
fn own_data_and_shared_skills_have_distinct_access_from_host_private_files() {
    let (dir, p) = fixture();
    for path in ["config.toml", "data/new/file", "skills/a/SKILL.md"] {
        assert!(p.resolve(&p.user_root.join(path), true).is_ok());
    }
    assert!(
        p.resolve(&p.host_root.join("skills/a/SKILL.md"), false)
            .is_ok()
    );
    assert!(
        p.resolve(&p.host_root.join("skills/a/SKILL.md"), true)
            .is_err()
    );
    for path in [
        "users/bob/config.toml",
        "users/alice-other/data",
        "host.toml",
        "run/runtime.json",
        "backups/old/data",
    ] {
        for write in [false, true] {
            assert!(p.resolve(&p.host_root.join(path), write).is_err(), "{path}");
        }
    }
    assert!(p.resolve(&p.host_root, true).is_err());
    assert!(p.resolve(dir.path(), true).is_err());
    assert!(p.resolve(&dir.path().join("public/file"), true).is_ok());
    assert!(p.resolve(&dir.path().join("tls.key"), false).is_err());
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let excludes = p.search_exclusions(&root).unwrap();
    assert_eq!(excludes.len(), 2);
}

#[cfg(unix)]
#[test]
fn symlinks_and_missing_descendants_cannot_bypass_either_side_of_the_boundary() {
    use std::os::unix::fs::symlink;
    let (dir, p) = fixture();
    let public = dir.path().join("public");
    std::fs::create_dir(&public).unwrap();
    symlink(p.host_root.join("users/bob"), public.join("other")).unwrap();
    symlink(&public, p.host_root.join("users/bob/out")).unwrap();
    symlink(p.host_root.join("users/bob"), p.user_root.join("other")).unwrap();
    symlink(dir.path().join("missing"), p.user_root.join("dangling")).unwrap();
    for path in [
        public.join("other/new/file"),
        p.host_root.join("users/bob/out/file"),
        p.user_root.join("other/data"),
        p.user_root.join("dangling/new"),
    ] {
        assert!(p.resolve(&path, false).is_err(), "{path:?}");
        assert!(p.resolve(&path, true).is_err(), "{path:?}");
    }
    symlink(&public, p.user_root.join("workspace")).unwrap();
    assert!(
        p.resolve(&p.user_root.join("workspace/new/file"), true)
            .is_ok()
    );
}

#[test]
fn pending_tls_key_changes_apply_to_an_existing_user_policy() {
    let (dir, policy) = fixture();
    let pending = dir.path().join("next.key");
    assert!(policy.resolve(&pending, false).is_ok());
    policy
        .protected_files
        .write()
        .unwrap()
        .push(pending.clone());
    assert!(policy.resolve(&pending, false).is_err());
    assert!(policy.resolve(&dir.path().join("tls.key"), false).is_err());
}
