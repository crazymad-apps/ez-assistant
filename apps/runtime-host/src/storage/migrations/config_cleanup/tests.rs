use std::os::unix::fs::{MetadataExt, PermissionsExt};

use assistant_runtime::{ConfigSourceFuture, ConfigSourceReplaceFuture};

use super::*;
use crate::config_source::LocalConfigSource;

const LEGACY: &str = r#"# user configuration
schema_version = 1
default_model = [false, 12] # invalid old value is discarded
models = { old = { api_key = "fixture-secret", unexpected = true } }
[runtime.model_transport]
request_timeout_ms = 9876 # keep this comment
[agent.vision]
model_key = { invalid = "old" }
timeout_ms = 8000
max_output_tokens = 1000
[speech]
models = ["keep"]
default_model = "speech-default"
[host_access]
port = 9999
"#;

#[tokio::test]
async fn cleanup_preserves_other_values_and_is_byte_stable_on_repeated_startup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, LEGACY).unwrap();
    let source = LocalConfigSource::new(path.clone());
    let backup = cleanup(&source, directory.path()).await.unwrap().unwrap();
    assert_eq!(fs::read_to_string(&backup).unwrap(), LEGACY);
    assert_eq!(
        fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let cleaned = fs::read_to_string(&path).unwrap();
    let document: toml::Value = toml::from_str(&cleaned).unwrap();
    assert!(document.get("default_model").is_none());
    assert!(document.get("models").is_none());
    assert!(document["agent"]["vision"].get("model_key").is_none());
    assert_eq!(
        document["agent"]["vision"]["timeout_ms"].as_integer(),
        Some(8000)
    );
    assert_eq!(
        document["agent"]["vision"]["max_output_tokens"].as_integer(),
        Some(1000)
    );
    assert_eq!(
        document["speech"]["default_model"].as_str(),
        Some("speech-default")
    );
    assert_eq!(document["speech"]["models"][0].as_str(), Some("keep"));
    assert_eq!(document["host_access"]["port"].as_integer(), Some(9999));
    assert!(cleaned.contains("# keep this comment"));
    assert!(!cleaned.contains("fixture-secret"));
    let inode = fs::metadata(&path).unwrap().ino();
    assert!(cleanup(&source, directory.path()).await.unwrap().is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), cleaned);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(
        fs::read_dir(backup.parent().unwrap().parent().unwrap())
            .unwrap()
            .count(),
        1
    );
    assert!(!directory.path().join("data").exists());
}

#[test]
fn all_toml_table_forms_remove_only_the_deprecated_path() {
    for contents in [
        "agent = { vision = { model_key = 9, timeout_ms = 77 }, model_key = 'keep' }\n",
        "agent.vision.model_key = ['invalid']\nagent.vision.timeout_ms = 77\nagent.model_key = 'keep'\n",
        "[agent]\nmodel_key = 'keep'\nvision = { model_key = false, timeout_ms = 77 }\n",
    ] {
        let cleaned = remove_legacy_keys(contents).unwrap().unwrap();
        let document: toml::Value = toml::from_str(&cleaned).unwrap();
        assert!(document["agent"]["vision"].get("model_key").is_none());
        assert_eq!(
            document["agent"]["vision"]["timeout_ms"].as_integer(),
            Some(77)
        );
        assert_eq!(document["agent"]["model_key"].as_str(), Some("keep"));
    }
    for contents in ["agent = false\n", "[agent]\nvision = 99\n", "# unchanged\n"] {
        assert!(remove_legacy_keys(contents).unwrap().is_none());
    }
}

#[tokio::test]
async fn invalid_syntax_missing_and_unsafe_files_are_never_rewritten() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let source = LocalConfigSource::new(path.clone());
    assert!(cleanup(&source, directory.path()).await.unwrap().is_none());
    let broken = "default_model = [\n[models.secret]\napi_key = 'preserve'\n";
    fs::write(&path, broken).unwrap();
    assert!(matches!(
        cleanup(&source, directory.path()).await,
        Err(CleanupError::InvalidSyntax)
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), broken);
    assert!(!directory.path().join("backups").exists());
    fs::remove_file(&path).unwrap();
    let target = directory.path().join("target");
    fs::write(&target, LEGACY).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(matches!(
        cleanup(&source, directory.path()).await,
        Err(CleanupError::Unavailable)
    ));
    assert_eq!(fs::read_to_string(&target).unwrap(), LEGACY);
}

struct EditedBeforeReplace {
    source: LocalConfigSource,
    path: PathBuf,
}
impl RuntimeConfigSource for EditedBeforeReplace {
    fn load(&self) -> ConfigSourceFuture<'_> {
        self.source.load()
    }
    fn replace(&self, revision: Option<String>, document: String) -> ConfigSourceReplaceFuture<'_> {
        Box::pin(async move {
            fs::write(&self.path, "host_access.port = 12345\n").unwrap();
            self.source.replace(revision, document).await
        })
    }
}

#[tokio::test]
async fn concurrent_edit_is_preserved_and_the_original_backup_remains_readable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, LEGACY).unwrap();
    let source = EditedBeforeReplace {
        source: LocalConfigSource::new(path.clone()),
        path: path.clone(),
    };
    assert!(matches!(
        cleanup(&source, directory.path()).await,
        Err(CleanupError::Conflict)
    ));
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "host_access.port = 12345\n"
    );
    let backups: Vec<_> = fs::read_dir(directory.path().join("backups/configuration"))
        .unwrap()
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        fs::read_to_string(backups[0].as_ref().unwrap().path().join("config.toml")).unwrap(),
        LEGACY
    );
}

#[tokio::test]
async fn backup_failure_stops_before_replacing_the_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, LEGACY).unwrap();
    fs::write(directory.path().join("backups"), "obstruction").unwrap();
    let source = LocalConfigSource::new(path.clone());
    assert!(matches!(
        cleanup(&source, directory.path()).await,
        Err(CleanupError::Backup)
    ));
    assert_eq!(fs::read_to_string(path).unwrap(), LEGACY);
}
