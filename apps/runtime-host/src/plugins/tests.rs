use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use super::archive::{ArchiveFormat, prepare_archive};
use super::paths::PluginPaths;

fn test_root() -> tempfile::TempDir {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("c03-m0-package-")
        .tempdir_in(base)
        .unwrap()
}

fn manifest() -> String {
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        "linux" => "linux",
        other => panic!("unsupported test OS: {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => panic!("unsupported test arch: {other}"),
    };
    serde_json::json!({
        "format_version": 1,
        "name": "document-tools",
        "display_name": "文档工具",
        "description": "本地文档",
        "version": "1.0.0",
        "platforms": [{"os": os, "arch": arch}],
        "min_host_version": "0.28.0",
        "skills": ["document-review"],
        "mcp": [],
        "configuration": [],
        "external_software": []
    })
    .to_string()
}

fn files() -> Vec<(String, Vec<u8>)> {
    vec![
        ("plugin.json".to_owned(), manifest().into_bytes()),
        (
            "skills/document-review/SKILL.md".to_owned(),
            b"---\nname: document-review\ndescription: Read documents\n---\nRead files.".to_vec(),
        ),
    ]
}

fn files_with_mcp() -> Vec<(String, Vec<u8>)> {
    let mut entries = files();
    let mut parsed: serde_json::Value = serde_json::from_slice(&entries[0].1).unwrap();
    parsed["mcp"] = serde_json::json!(["document-reader"]);
    entries[0].1 = parsed.to_string().into_bytes();
    let os = parsed["platforms"][0]["os"].as_str().unwrap();
    let arch = parsed["platforms"][0]["arch"].as_str().unwrap();
    let runtime = format!("runtimes/{os}-{arch}/node");
    entries.push((
        "mcp/document-reader/server.json".to_owned(),
        serde_json::json!({"mcpServers": {"document-reader": {
            "command": format!("${{plugin_root}}/{runtime}"),
            "args": ["${plugin_root}/mcp/document-reader/main.js"]
        }}})
        .to_string()
        .into_bytes(),
    ));
    entries.push((
        "mcp/document-reader/main.js".to_owned(),
        b"process.exit(0)".to_vec(),
    ));
    entries.push((runtime, b"#!/bin/sh\nexit 0\n".to_vec()));
    entries
}

fn zip(path: &Path, entries: &[(String, Vec<u8>)]) {
    let mut writer = zip::ZipWriter::new(File::create(path).unwrap());
    for (name, bytes) in entries {
        let mode = if name.starts_with("runtimes/") {
            0o755
        } else {
            0o644
        };
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(mode);
        writer.start_file(name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

fn tar_gzip(path: &Path, entries: &[(String, Vec<u8>)]) {
    let encoder =
        flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::default());
    let mut writer = tar::Builder::new(encoder);
    for (name, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(if name.starts_with("runtimes/") {
            0o755
        } else {
            0o644
        });
        header.set_cksum();
        writer
            .append_data(&mut header, name, bytes.as_slice())
            .unwrap();
    }
    writer.finish().unwrap();
}

fn zip_link(path: &Path, target: &str) {
    let mut entries = files();
    entries.push(("resources/link".into(), target.as_bytes().to_vec()));
    zip(path, &entries);
    // ZipWriter::start_file always marks entries regular; set the central Unix file type.
    let mut bytes = fs::read(path).unwrap();
    let headers: Vec<_> = bytes
        .windows(4)
        .enumerate()
        .filter_map(|(index, item)| (item == b"PK\x01\x02").then_some(index))
        .collect();
    let offset = headers.last().unwrap() + 38;
    bytes[offset..offset + 4].copy_from_slice(&(0o120777_u32 << 16).to_le_bytes());
    fs::write(path, bytes).unwrap();
}

fn tar_link(path: &Path, target: &str) {
    let encoder =
        flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::default());
    let mut writer = tar::Builder::new(encoder);
    for (name, bytes) in files() {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        writer
            .append_data(&mut header, name, bytes.as_slice())
            .unwrap();
    }
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o777);
    header.set_link_name(target).unwrap();
    header.set_cksum();
    writer
        .append_data(&mut header, "resources/link", std::io::empty())
        .unwrap();
    writer.finish().unwrap();
}

#[test]
fn both_archives_prepare_the_same_package_without_publishing() {
    let root = test_root();
    for (file, format) in [
        ("valid.zip", ArchiveFormat::Zip),
        ("valid.tar.gz", ArchiveFormat::TarGzip),
    ] {
        let source = root.path().join(file);
        match format {
            ArchiveFormat::Zip => zip(&source, &files()),
            ArchiveFormat::TarGzip => tar_gzip(&source, &files()),
        }
        let prepared = prepare_archive(&source, format, root.path()).unwrap();
        assert_eq!(prepared.manifest.name, "document-tools");
        assert_eq!(prepared.sha256.len(), 64);
        assert!(
            prepared
                .staging
                .path()
                .join("skills/document-review/SKILL.md")
                .is_file()
        );
        prepared.staging.close().unwrap();
    }
}

#[test]
fn both_archives_validate_declared_mcp_entry_and_runtime() {
    let root = test_root();
    for (file, format) in [
        ("mcp.zip", ArchiveFormat::Zip),
        ("mcp.tar.gz", ArchiveFormat::TarGzip),
    ] {
        let source = root.path().join(file);
        match format {
            ArchiveFormat::Zip => zip(&source, &files_with_mcp()),
            ArchiveFormat::TarGzip => tar_gzip(&source, &files_with_mcp()),
        }
        let prepared = prepare_archive(&source, format, root.path()).unwrap();
        assert_eq!(prepared.manifest.mcp, ["document-reader"]);
        prepared.staging.close().unwrap();
    }
}

#[test]
fn archive_rejects_undeclared_skill_and_leaves_no_prepared_package() {
    let root = test_root();
    let mut entries = files();
    entries.push((
        "skills/undeclared/SKILL.md".to_owned(),
        b"untrusted".to_vec(),
    ));
    let source = root.path().join("invalid.zip");
    zip(&source, &entries);
    assert!(prepare_archive(&source, ArchiveFormat::Zip, root.path()).is_err());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn archive_rejects_path_escape_case_collision_and_corrupt_crc() {
    let root = test_root();
    for (file, extra) in [
        ("escape.zip", "resources/../outside"),
        ("collision.zip", "resources/README"),
    ] {
        let mut entries = files();
        entries.push((extra.to_owned(), b"untrusted".to_vec()));
        if file == "collision.zip" {
            entries.push(("resources/Readme".to_owned(), b"duplicate".to_vec()));
        }
        let source = root.path().join(file);
        zip(&source, &entries);
        assert!(
            prepare_archive(&source, ArchiveFormat::Zip, root.path()).is_err(),
            "{file}"
        );
    }
    let source = root.path().join("crc.zip");
    zip(&source, &files());
    let mut raw = fs::read(&source).unwrap();
    let start = raw
        .windows(b"Read files.".len())
        .position(|bytes| bytes == b"Read files.")
        .unwrap();
    raw[start] ^= 1;
    fs::write(&source, raw).unwrap();
    assert!(prepare_archive(&source, ArchiveFormat::Zip, root.path()).is_err());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 3);
}

#[test]
fn links_stay_inside_package_and_invalid_links_leave_no_staging() {
    let root = test_root();
    for (file, format, target, accepted) in [
        (
            "valid-link.zip",
            ArchiveFormat::Zip,
            "../skills/document-review/SKILL.md",
            true,
        ),
        (
            "escape-link.zip",
            ArchiveFormat::Zip,
            "../../outside",
            false,
        ),
        (
            "valid-link.tar.gz",
            ArchiveFormat::TarGzip,
            "../skills/document-review/SKILL.md",
            true,
        ),
        (
            "escape-link.tar.gz",
            ArchiveFormat::TarGzip,
            "../../outside",
            false,
        ),
    ] {
        let source = root.path().join(file);
        match format {
            ArchiveFormat::Zip => zip_link(&source, target),
            ArchiveFormat::TarGzip => tar_link(&source, target),
        }
        let prepared = prepare_archive(&source, format, root.path());
        assert_eq!(prepared.is_ok(), accepted, "{file}");
        if let Ok(prepared) = prepared {
            #[cfg(unix)]
            assert!(
                fs::symlink_metadata(prepared.staging.path().join("resources/link"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            prepared.staging.close().unwrap();
        }
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 4);
}

#[test]
fn oversized_tar_metadata_is_rejected_before_parsing() {
    let root = test_root();
    let path = root.path().join("large-pax.tar.gz");
    let encoder =
        flate2::write::GzEncoder::new(File::create(&path).unwrap(), flate2::Compression::default());
    let mut writer = tar::Builder::new(encoder);
    let body = vec![b'x'; 1024 * 1024 + 1];
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::XHeader);
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    writer
        .append_data(&mut header, "pax-header", body.as_slice())
        .unwrap();
    writer.finish().unwrap();
    assert!(prepare_archive(&path, ArchiveFormat::TarGzip, root.path()).is_err());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn host_rejects_unsupported_platform_and_newer_minimum_version() {
    let root = test_root();
    for (file, change) in ["other-platform.zip", "future-host.zip"]
        .into_iter()
        .zip(["platform", "version"])
    {
        let mut entries = files();
        let mut manifest: serde_json::Value = serde_json::from_slice(&entries[0].1).unwrap();
        if change == "platform" {
            let other = if std::env::consts::OS == "macos" {
                "linux"
            } else {
                "macos"
            };
            manifest["platforms"][0]["os"] = serde_json::json!(other);
        } else {
            manifest["min_host_version"] = serde_json::json!("99.0.0");
        }
        entries[0].1 = manifest.to_string().into_bytes();
        let archive = root.path().join(file);
        zip(&archive, &entries);
        assert!(
            prepare_archive(&archive, ArchiveFormat::Zip, root.path()).is_err(),
            "{file}"
        );
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
}

/// 只读消费用户已有的中心样例包；复制到新的隔离目录后完成 Host 独立准入。
#[test]
#[ignore = "set C03_SAMPLE_ARCHIVE, C03_SAMPLE_FORMAT and C03_SAMPLE_EXPECT_VALID"]
fn preserved_local_sample_follows_package_policy_without_changing_its_source() {
    let source =
        PathBuf::from(std::env::var_os("C03_SAMPLE_ARCHIVE").expect("sample archive path"));
    let format = match std::env::var("C03_SAMPLE_FORMAT").as_deref() {
        Ok("zip") => ArchiveFormat::Zip,
        Ok("tar_gzip") => ArchiveFormat::TarGzip,
        _ => panic!("expected C03_SAMPLE_FORMAT=zip or tar_gzip"),
    };
    let source_sha = Sha256::digest(fs::read(&source).unwrap());
    let root = test_root();
    let copied = root.path().join(match format {
        ArchiveFormat::Zip => "sample.zip",
        ArchiveFormat::TarGzip => "sample.tar.gz",
    });
    fs::copy(&source, &copied).unwrap();
    let prepared = prepare_archive(&copied, format, root.path());
    let expected_valid = match std::env::var("C03_SAMPLE_EXPECT_VALID").as_deref() {
        Ok("true") => true,
        Ok("false") => false,
        _ => panic!("expected C03_SAMPLE_EXPECT_VALID=true or false"),
    };
    assert_eq!(prepared.is_ok(), expected_valid);
    if !expected_valid {
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        assert_eq!(Sha256::digest(fs::read(&source).unwrap()), source_sha);
        return;
    }
    let prepared = prepared.unwrap();
    assert_eq!(prepared.manifest.name, "warehouse-cad-guidance");
    assert_eq!(prepared.manifest.skills, ["compact-shelving-cad"]);
    assert_eq!(prepared.manifest.mcp.len(), 0);
    if let Some(filename) = source.file_name().and_then(|name| name.to_str())
        && let Some(sha) = filename.strip_suffix(".tar.gz")
        && sha.len() == 64
    {
        assert_eq!(prepared.sha256, sha);
    }
    prepared.staging.close().unwrap();
    assert_eq!(Sha256::digest(fs::read(&source).unwrap()), source_sha);
}

#[test]
fn managed_paths_stay_in_their_user_domain() {
    let root = test_root();
    let alice_root = root.path().join("users/alice");
    let bob_root = root.path().join("users/bob");
    fs::create_dir_all(&alice_root).unwrap();
    fs::create_dir_all(&bob_root).unwrap();
    let alice = PluginPaths::prepare(&alice_root).unwrap();
    let bob = PluginPaths::prepare(&bob_root).unwrap();
    alice.ensure_plugin("document-tools").unwrap();
    let sha = "a".repeat(64);
    let alice_package = alice.package("document-tools", &sha).unwrap();
    let bob_package = bob.package("document-tools", &sha).unwrap();
    assert_ne!(alice_package, bob_package);
    assert!(alice.config("document-tools").unwrap().is_dir());
    assert!(alice.data("document-tools").unwrap().is_dir());
    assert!(alice.package("../bob", &sha).is_err());
    assert!(alice.package("document-tools", "../bad").is_err());
    let key = assistant_protocol::IdempotencyKey::new("arbitrary/user key").unwrap();
    assert!(
        alice
            .operation(&key, false)
            .starts_with(alice_root.join("plugins/.staging"))
    );
}

/// 与 C01 使用完全相同的归档字节比对静态准入；单独运行前需构建 enterprise-center。
#[test]
#[ignore = "requires Node 24 and built enterprise-center dist"]
fn center_and_host_agree_on_shared_archive_bytes() {
    let root = test_root();
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let center = project.join("apps/enterprise-center/dist/plugins/archive.js");
    assert!(
        center.is_file(),
        "build enterprise-center before conformance test"
    );
    let scan = r#"
        import { scanArchive } from './apps/enterprise-center/dist/plugins/archive.js';
        try {
          await scanArchive(process.argv[1], process.argv[2], new AbortController().signal);
        } catch { process.exitCode = 1; }
    "#;
    for (name, format, entries, accepted) in [
        ("valid.zip", ArchiveFormat::Zip, files_with_mcp(), true),
        (
            "valid.tar.gz",
            ArchiveFormat::TarGzip,
            files_with_mcp(),
            true,
        ),
        (
            "undeclared.zip",
            ArchiveFormat::Zip,
            {
                let mut files = files();
                files.push(("skills/other/SKILL.md".into(), b"untrusted".to_vec()));
                files
            },
            false,
        ),
        (
            "escape.zip",
            ArchiveFormat::Zip,
            {
                let mut files = files();
                files.push(("resources/../escape".into(), b"untrusted".to_vec()));
                files
            },
            false,
        ),
    ] {
        let path = root.path().join(name);
        match format {
            ArchiveFormat::Zip => zip(&path, &entries),
            ArchiveFormat::TarGzip => tar_gzip(&path, &entries),
        }
        let host = prepare_archive(&path, format, root.path());
        let center = std::process::Command::new("node")
            .current_dir(&project)
            .args(["--input-type=module", "-e", scan])
            .arg(&path)
            .arg(match format {
                ArchiveFormat::Zip => "zip",
                ArchiveFormat::TarGzip => "tar_gzip",
            })
            .output()
            .expect("run Center scanner");
        assert_eq!(host.is_ok(), accepted, "Host: {name}");
        assert_eq!(
            center.status.success(),
            accepted,
            "Center: {name}; {}",
            String::from_utf8_lossy(&center.stderr)
        );
        if let Ok(prepared) = host {
            prepared.staging.close().unwrap();
        }
    }
    for (name, format, target, accepted) in [
        (
            "valid-link.zip",
            ArchiveFormat::Zip,
            "../skills/document-review/SKILL.md",
            true,
        ),
        (
            "escape-link.zip",
            ArchiveFormat::Zip,
            "../../outside",
            false,
        ),
        (
            "valid-link.tar.gz",
            ArchiveFormat::TarGzip,
            "../skills/document-review/SKILL.md",
            true,
        ),
        (
            "escape-link.tar.gz",
            ArchiveFormat::TarGzip,
            "../../outside",
            false,
        ),
    ] {
        let path = root.path().join(name);
        match format {
            ArchiveFormat::Zip => zip_link(&path, target),
            ArchiveFormat::TarGzip => tar_link(&path, target),
        }
        let host = prepare_archive(&path, format, root.path());
        let center = std::process::Command::new("node")
            .current_dir(&project)
            .args(["--input-type=module", "-e", scan])
            .arg(&path)
            .arg(match format {
                ArchiveFormat::Zip => "zip",
                ArchiveFormat::TarGzip => "tar_gzip",
            })
            .output()
            .expect("run Center scanner");
        assert_eq!(host.is_ok(), accepted, "Host: {name}");
        assert_eq!(
            center.status.success(),
            accepted,
            "Center: {name}; {}",
            String::from_utf8_lossy(&center.stderr)
        );
        if let Ok(prepared) = host {
            prepared.staging.close().unwrap();
        }
    }
}
