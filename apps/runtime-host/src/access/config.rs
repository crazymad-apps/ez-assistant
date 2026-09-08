//! 与模型设置共享同一配置来源及 CAS，只编辑 Host 所有的访问节。

use std::path::Path;

use assistant_protocol::{HostAccessConfiguration, HostAccessScheme};
use assistant_runtime::{ConfigSourceLoad, ConfigSourceReplace, RuntimeConfigSource};
use serde::{Deserialize, Serialize};
use toml_edit::DocumentMut;

use super::{AccessError, credentials::validate_hash};

#[derive(Clone, Default, Deserialize, Serialize)]
pub(super) struct AccessConfiguration {
    pub(super) password_hash: Option<String>,
    #[serde(flatten)]
    pub(super) public: HostAccessConfiguration,
}

pub(super) struct AccessDocument {
    pub(super) revision: Option<String>,
    pub(super) document: DocumentMut,
    pub(super) access: AccessConfiguration,
}

pub(super) async fn load(source: &dyn RuntimeConfigSource) -> Result<AccessDocument, AccessError> {
    let (document, revision) = match source.load().await {
        ConfigSourceLoad::Missing => (
            "schema_version = 1\ndefault_model = \"\"\n"
                .parse::<DocumentMut>()
                .expect("static TOML"),
            None,
        ),
        ConfigSourceLoad::Document(document) => (
            document
                .contents()
                .parse::<DocumentMut>()
                .map_err(|_| AccessError::Invalid("配置文件不是有效 TOML。"))?,
            Some(document.revision().to_owned()),
        ),
        ConfigSourceLoad::Unavailable(_) => return Err(AccessError::Unavailable),
    };
    let access = match document.get("host_access") {
        Some(section) => toml::from_str::<AccessConfiguration>(&section.to_string())
            .map_err(|_| AccessError::Invalid("Host 访问配置无效。"))?,
        None => AccessConfiguration::default(),
    };
    if let Some(hash) = &access.password_hash {
        validate_hash(hash)?;
    }
    Ok(AccessDocument {
        document,
        revision,
        access,
    })
}

pub(super) async fn save(
    source: &dyn RuntimeConfigSource,
    mut loaded: AccessDocument,
) -> Result<AccessDocument, AccessError> {
    let section = toml::to_string(&loaded.access)
        .map_err(|_| AccessError::Unavailable)?
        .parse::<DocumentMut>()
        .map_err(|_| AccessError::Unavailable)?;
    loaded.document["host_access"] = toml_edit::Item::Table(section.as_table().clone());
    match source
        .replace(loaded.revision, loaded.document.to_string())
        .await
    {
        ConfigSourceReplace::Applied(document) => {
            loaded.revision = Some(document.revision().to_owned());
            Ok(loaded)
        }
        ConfigSourceReplace::Conflict(_) => Err(AccessError::Conflict),
        ConfigSourceReplace::Unavailable(_) => Err(AccessError::Unavailable),
    }
}

pub(super) fn validate(configuration: &HostAccessConfiguration) -> Result<(), AccessError> {
    if configuration.port == 0 {
        return Err(AccessError::Invalid("端口必须为 1—65535。"));
    }
    if configuration.server_names.len() > 16 {
        return Err(AccessError::Invalid("最多可设置 16 个访问域名。"));
    }
    for name in &configuration.server_names {
        let parsed = reqwest::Url::parse(&format!("http://{name}"))
            .map_err(|_| AccessError::Invalid("访问域名无效。"))?;
        if parsed.host_str() != Some(name.as_str())
            || name.parse::<std::net::IpAddr>().is_ok()
            || name.starts_with('[')
            || parsed.port().is_some()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(AccessError::Invalid(
                "请填写小写域名，不包含协议、端口或路径；IP 无需填写。",
            ));
        }
    }
    if configuration.scheme == HostAccessScheme::Https {
        for path in [
            &configuration.tls_certificate,
            &configuration.tls_private_key,
        ] {
            if path
                .as_ref()
                .is_none_or(|path| !Path::new(path).is_absolute())
            {
                return Err(AccessError::Invalid(
                    "HTTPS 需要 Host 上证书和私钥的绝对路径。",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn same_endpoint(
    left: &HostAccessConfiguration,
    right: &HostAccessConfiguration,
) -> bool {
    left.port == right.port
        && left.scheme == right.scheme
        && left.tls_certificate == right.tls_certificate
        && left.tls_private_key == right.tls_private_key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_source::LocalConfigSource;

    #[test]
    fn a_port_override_keeps_remote_access_closed_by_default() {
        let configuration: AccessConfiguration = toml::from_str("port = 7241").unwrap();
        assert_eq!(configuration.public.port, 7241);
        assert!(!configuration.public.remote_enabled);
        assert!(configuration.password_hash.is_none());
        assert!(validate(&configuration.public).is_ok());
    }

    #[tokio::test]
    async fn access_settings_preserve_other_configuration_and_detect_conflicts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, "# user settings\nschema_version = 1\ndefault_model = \"sample\"\n[models.sample]\nmodel = \"example\"\n").unwrap();
        let source = LocalConfigSource::new(path.clone());
        let mut first = load(&source).await.unwrap();
        let second = load(&source).await.unwrap();
        first.access.public.port = 9999;
        save(&source, first).await.unwrap();
        assert!(matches!(
            save(&source, second).await,
            Err(AccessError::Conflict)
        ));
        let contents = std::fs::read_to_string(path).unwrap();
        assert!(contents.contains("# user settings"));
        assert!(contents.contains("[models.sample]"));
        assert_eq!(load(&source).await.unwrap().access.public.port, 9999);
    }

    #[test]
    fn server_names_are_domains_and_http_does_not_require_certificates() {
        let mut configuration = HostAccessConfiguration {
            remote_enabled: true,
            server_names: vec!["runtime.example".into()],
            ..Default::default()
        };
        assert!(validate(&configuration).is_ok());
        for invalid in [
            "http://runtime.example",
            "runtime.example:7240",
            "runtime.example/path",
            "runtime.example?x=1",
            "user@runtime.example",
            "127.0.0.1",
            "RUNTIME.EXAMPLE",
        ] {
            configuration.server_names = vec![invalid.into()];
            assert!(validate(&configuration).is_err(), "{invalid}");
        }
    }
}
