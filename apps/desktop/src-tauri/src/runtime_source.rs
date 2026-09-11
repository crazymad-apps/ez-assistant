//! Native restart source verification; neither Client installation layout nor its runtime is consulted.

use assistant_protocol::ClientCompatibility;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt as _;

pub(crate) async fn verify(
    path: &Path,
    expected_hash: &str,
    expected: &ClientCompatibility,
) -> Result<(), ()> {
    let check = |path: PathBuf| tokio::task::spawn_blocking(move || digest(&path));
    if check(path.to_owned()).await.map_err(|_| ())?? != expected_hash {
        return Err(());
    }
    let mut child = tokio::process::Command::new(path)
        .arg("--build-info-json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ())?;
    let mut output = child.stdout.take().ok_or(())?.take(4097);
    let version = tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).await.map_err(|_| ())?;
        if bytes.len() > 4096 || !child.wait().await.map_err(|_| ())?.success() {
            return Err(());
        }
        serde_json::from_slice::<ClientCompatibility>(&bytes).map_err(|_| ())
    })
    .await
    .map_err(|_| ())??;
    if &version != expected || !version.is_valid() {
        return Err(());
    }
    if check(path.to_owned()).await.map_err(|_| ())?? != expected_hash {
        return Err(());
    }
    Ok(())
}

pub(crate) fn digest(path: &Path) -> Result<String, ()> {
    if !path.is_absolute() || path.canonicalize().map_err(|_| ())? != path {
        return Err(());
    }
    let mut file = File::open(path).map_err(|_| ())?;
    let before = file.metadata().map_err(|_| ())?;
    if !before.is_file() {
        return Err(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if before.mode() & 0o111 == 0 {
            return Err(());
        }
    }
    let mut digest = Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut bytes).map_err(|_| ())?;
        if count == 0 {
            break;
        }
        digest.update(&bytes[..count]);
    }
    let after = std::fs::metadata(path).map_err(|_| ())?;
    if before.len() != after.len()
        || before.modified().map_err(|_| ())? != after.modified().map_err(|_| ())?
    {
        return Err(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(());
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Matches Host path semantics without relying on a Client executable or creating directories.
pub(crate) fn canonical_home(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    if !path.is_absolute() {
        return None;
    }
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            component => {
                resolved.push(component.as_os_str());
                match std::fs::canonicalize(&resolved) {
                    Ok(path) => resolved = path,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            && std::fs::symlink_metadata(&resolved).is_err_and(|error| {
                                error.kind() == std::io::ErrorKind::NotFound
                            }) => {}
                    Err(_) => return None,
                }
            }
        }
    }
    Some(resolved)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    fn script(root: &Path, name: &str, body: &str) -> PathBuf {
        let path = root.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path.canonicalize().unwrap()
    }

    #[tokio::test]
    async fn restart_source_requires_matching_digest_and_software_pair() {
        let temp = tempfile::tempdir().unwrap();
        let expected = ClientCompatibility::current();
        let body = format!(
            "printf '%s' '{}'",
            serde_json::to_string(&expected).unwrap()
        );
        let path = script(temp.path(), "host", &body);
        let hash = digest(&path).unwrap();
        assert!(verify(&path, &hash, &expected).await.is_ok());
        assert!(verify(&path, "wrong", &expected).await.is_err());
        let different = ClientCompatibility {
            version: "0.25.3".into(),
            min_compatible_version: "0.25.2".into(),
        };
        assert!(verify(&path, &hash, &different).await.is_err());
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        assert!(verify(&path, &hash, &expected).await.is_err());
        fs::remove_file(&path).unwrap();
        assert!(verify(&path, &hash, &expected).await.is_err());
    }

    #[tokio::test]
    async fn build_info_must_be_bounded_successful_and_leave_source_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let expected = ClientCompatibility::current();
        let json = serde_json::to_string(&expected).unwrap();
        for (name, body) in [
            ("failed", format!("printf '%s' '{json}'; exit 1")),
            (
                "oversized",
                "i=0; while [ $i -lt 5000 ]; do printf x; i=$((i+1)); done".into(),
            ),
            (
                "mutating",
                format!("printf '%s' '{json}'; printf '\\n# changed' >> \"$0\""),
            ),
        ] {
            let path = script(temp.path(), name, &body);
            assert!(
                verify(&path, &digest(&path).unwrap(), &expected)
                    .await
                    .is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn executable_alias_is_rejected_but_home_alias_has_one_canonical_identity() {
        let temp = tempfile::tempdir().unwrap();
        let path = script(temp.path(), "host", "exit 0");
        let alias = temp.path().join("host-alias");
        symlink(&path, &alias).unwrap();
        assert!(digest(&alias).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(digest(&path).is_err());
        let home = temp.path().join("home");
        fs::create_dir(&home).unwrap();
        let home_alias = temp.path().join("home-alias");
        symlink(&home, &home_alias).unwrap();
        assert_eq!(
            canonical_home(&home_alias.join("missing/../next")),
            Some(home.canonicalize().unwrap().join("next"))
        );
        assert!(!home.join("next").exists());
        let dangling = temp.path().join("dangling");
        symlink(temp.path().join("absent"), &dangling).unwrap();
        assert!(canonical_home(&dangling).is_none());
    }
}
