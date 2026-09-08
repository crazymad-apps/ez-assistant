use super::*;
use std::{
    fs,
    os::unix::{ffi::OsStringExt, fs::symlink},
};

#[test]
fn host_listing_is_unregistered_hidden_bounded_and_reports_invalid_names() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("sub")).unwrap();
    fs::write(temp.path().join(".hidden"), "hidden").unwrap();
    // APFS 不接受非 UTF-8 文件名；Linux 上实际覆盖跳过行为，macOS 记录文件系统拒绝。
    let invalid_name_created = fs::write(
        temp.path().join(std::ffi::OsString::from_vec(vec![0xff])),
        "invalid",
    )
    .is_ok();
    let path = host_path(temp.path().to_str()).unwrap();
    let listing = list(&path, None, false, true).unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].kind, SessionResourceEntryKind::Directory);
    assert_eq!(listing.skipped_entries, u32::from(invalid_name_created));
    assert_eq!(list(&path, None, true, true).unwrap().entries.len(), 2);
    for index in 0..MAX_DIRECTORY_ENTRIES {
        fs::write(path.join(format!("entry-{index}")), "").unwrap();
    }
    let listing = list(&path, None, true, true).unwrap();
    assert!(listing.truncated);
    assert!(listing.entries.len() <= MAX_DIRECTORY_ENTRIES);
}

#[test]
fn symlink_is_allowed_for_host_but_not_outside_a_session_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir(&root).unwrap();
    fs::write(temp.path().join("outside"), "outside").unwrap();
    symlink(temp.path().join("outside"), root.join("link")).unwrap();
    let root = fs::canonicalize(root).unwrap();
    assert_eq!(
        list(&root, Some(&root), true, true).unwrap().entries[0].state,
        SessionResourceEntryState::OutsideRoot
    );
    let host = list(&root, None, true, true).unwrap();
    assert_eq!(host.entries[0].state, SessionResourceEntryState::Available);
    assert!(host.entries[0].is_symbolic_link);
    assert_eq!(
        preview(&host_path(Some(&host.entries[0].path)).unwrap())
            .unwrap()
            .text
            .as_deref(),
        Some("outside")
    );
}

#[test]
fn replacing_file_or_parent_with_symlink_cannot_change_an_opened_read() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    fs::create_dir(root.join("inside")).unwrap();
    fs::create_dir(root.join("outside")).unwrap();
    fs::write(root.join("inside/file"), "original").unwrap();
    fs::write(root.join("outside/file"), "external").unwrap();
    let path = root.join("inside/file");
    let file = open_resolved(&path, false).unwrap();
    fs::rename(root.join("inside"), root.join("moved")).unwrap();
    symlink(root.join("outside"), root.join("inside")).unwrap();
    assert!(open_resolved(&path, false).is_err());
    assert_eq!(read_bounded(file, 64).unwrap(), b"original");
    fs::remove_file(root.join("inside")).unwrap();
    fs::rename(root.join("moved"), root.join("inside")).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(root.join("outside/file"), &path).unwrap();
    assert!(open_resolved(&path, false).is_err());
}

#[test]
fn preview_is_sniffed_and_bounded_even_when_content_grows() {
    struct Growing;
    impl Read for Growing {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            buffer.fill(b'x');
            Ok(buffer.len())
        }
    }
    assert_eq!(
        read_bounded(Growing, 100).unwrap_err().code,
        RuntimeErrorCode::ResourceTooLarge
    );
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("fake.png");
    fs::write(&file, "plain text, despite suffix").unwrap();
    assert_eq!(
        preview(&fs::canonicalize(&file).unwrap()).unwrap().kind,
        SessionResourcePreviewKind::Text
    );
    fs::write(&file, b"binary\0content").unwrap();
    assert_eq!(
        preview(&fs::canonicalize(&file).unwrap()).unwrap_err().code,
        RuntimeErrorCode::ResourceNotPreviewable
    );
    assert!(host_path(Some("relative")).is_err());
    assert!(host_path(Some("file://other-machine/tmp/file")).is_err());
}
