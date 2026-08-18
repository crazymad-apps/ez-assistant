//! Runtime Home 内持久化的 Conversation Recall 引用签名密钥。

use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

const KEY_FILE: &str = "recall-reference.key";
const PRIVATE_FILE_MODE: u32 = 0o600;

pub(crate) fn load_or_create(runtime_home: &Path) -> io::Result<[u8; 32]> {
    let path = runtime_home.join(KEY_FILE);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "recall reference key is not a regular file",
                ));
            }
            let mut key = [0_u8; 32];
            let mut file = OpenOptions::new().read(true).open(path)?;
            file.read_exact(&mut key)?;
            let mut trailing = [0_u8; 1];
            if file.read(&mut trailing)? != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "recall reference key has an invalid length",
                ));
            }
            Ok(key)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => create(&path),
        Err(error) => Err(error),
    }
}

fn create(path: &Path) -> io::Result<[u8; 32]> {
    let mut key = [0_u8; 32];
    getrandom::fill(&mut key).map_err(io::Error::other)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)?;
    if let Err(error) = file.write_all(&key).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, symlink};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn key_is_persistent_and_private() {
        let home = TempDir::new().expect("Runtime Home");

        let created = load_or_create(home.path()).expect("create key");
        let loaded = load_or_create(home.path()).expect("reload key");

        assert_eq!(created, loaded);
        let metadata = fs::metadata(home.path().join(KEY_FILE)).expect("key metadata");
        assert_eq!(metadata.len(), 32);
        assert_eq!(metadata.mode() & 0o777, PRIVATE_FILE_MODE);
    }

    #[test]
    fn malformed_or_linked_keys_are_rejected() {
        let malformed_home = TempDir::new().expect("malformed Runtime Home");
        fs::write(malformed_home.path().join(KEY_FILE), [0_u8; 31]).expect("malformed key");
        assert_eq!(
            load_or_create(malformed_home.path())
                .expect_err("invalid key length")
                .kind(),
            io::ErrorKind::UnexpectedEof
        );

        let linked_home = TempDir::new().expect("linked Runtime Home");
        let target = linked_home.path().join("target.key");
        fs::write(&target, [0_u8; 32]).expect("target key");
        symlink(&target, linked_home.path().join(KEY_FILE)).expect("key symlink");
        assert_eq!(
            load_or_create(linked_home.path())
                .expect_err("symlink key")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
