//! Host 配置的版本准入与模式设置。用户业务配置和 Center 凭据不属于本文件。

mod center_url;

use serde::Deserialize;
use toml_edit::{DocumentMut, value};

pub(crate) const LAYOUT_VERSION: &str = "0.27.0";
pub(crate) const FILE: &str = "host.toml";

pub(crate) use assistant_protocol::HostMode;

/// 仅用于启动和本机配置管理；登录核实后的中心 ID 仍由 Host 持久化。
#[derive(Deserialize)]
pub(crate) struct HostConfiguration {
    pub(crate) version: String,
    pub(crate) mode: HostMode,
    pub(crate) enterprise: Option<EnterpriseConfiguration>,
}

#[derive(Deserialize)]
pub(crate) struct EnterpriseConfiguration {
    pub(crate) center_url: String,
    pub(crate) center_id: Option<String>,
}

pub(crate) fn parse(contents: &str) -> Result<HostConfiguration, &'static str> {
    let config: HostConfiguration = toml::from_str(contents).map_err(|_| "Host 配置无效。")?;
    if config.version != LAYOUT_VERSION {
        return Err("Host 目录版本不受当前程序支持。");
    }
    if config.mode == HostMode::Enterprise && config.enterprise.is_none() {
        return Err("企业配置缺少中心地址。");
    }
    if let Some(enterprise) = &config.enterprise {
        center_url::validate(&enterprise.center_url)?;
        if let Some(id) = &enterprise.center_id {
            let bytes = id.as_bytes();
            if bytes.len() != 36
                || bytes.iter().enumerate().any(|(i, b)| {
                    if matches!(i, 8 | 13 | 18 | 23) {
                        *b != b'-'
                    } else {
                        !b.is_ascii_digit() && !(b'a'..=b'f').contains(b)
                    }
                })
            {
                return Err("中心身份必须是小写标准 UUID。");
            }
        }
    }
    Ok(config)
}

pub(crate) fn personal_document() -> DocumentMut {
    let mut doc = DocumentMut::new();
    doc["version"] = value(LAYOUT_VERSION);
    doc["mode"] = value("personal");
    doc
}

/// 创建企业配置时才取构建默认值。已有企业配置始终先严格校验，换包不能自动改连中心。
pub(crate) fn set_mode(
    document: &mut DocumentMut,
    mode: HostMode,
    explicit_url: Option<&str>,
) -> Result<(), &'static str> {
    set_mode_with_default(
        document,
        mode,
        explicit_url,
        option_env!("EZ_ASSISTANT_DEFAULT_CENTER_URL"),
    )
}

fn set_mode_with_default(
    document: &mut DocumentMut,
    mode: HostMode,
    explicit_url: Option<&str>,
    default_url: Option<&str>,
) -> Result<(), &'static str> {
    parse(&document.to_string())?;
    if mode == HostMode::Enterprise {
        let existing = document
            .get("enterprise")
            .and_then(|item| item.get("center_url"))
            .and_then(|item| item.as_str());
        let url = explicit_url
            .or(existing)
            .or(default_url)
            .ok_or("请配置企业中心地址。");
        let url = url?.to_owned();
        center_url::validate(&url)?;
        if existing.is_some_and(|old| old != url)
            && document["enterprise"].get("center_id").is_some()
        {
            return Err("更换已绑定中心前须显式清除中心绑定。");
        }
        document["enterprise"]["center_url"] = value(url);
    } else if explicit_url.is_some() {
        return Err("中心地址只能用于企业配置。");
    }
    document["mode"] = value(match mode {
        HostMode::Personal => "personal",
        HostMode::Enterprise => "enterprise",
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_only_used_when_creating_enterprise_configuration() {
        let mut doc = personal_document();
        set_mode_with_default(
            &mut doc,
            HostMode::Enterprise,
            None,
            Some("https://a.example/center"),
        )
        .unwrap();
        assert_eq!(
            parse(&doc.to_string())
                .unwrap()
                .enterprise
                .unwrap()
                .center_url,
            "https://a.example/center"
        );
        doc["enterprise"]["center_id"] = value("01234567-89ab-4cde-8f01-23456789abcd");
        let before = doc.to_string();
        set_mode_with_default(
            &mut doc,
            HostMode::Enterprise,
            None,
            Some("https://new.example"),
        )
        .unwrap();
        assert_eq!(doc.to_string(), before);
        assert!(
            set_mode_with_default(
                &mut doc,
                HostMode::Enterprise,
                Some("https://new.example"),
                None
            )
            .is_err()
        );
        let mut explicit = personal_document();
        set_mode_with_default(
            &mut explicit,
            HostMode::Enterprise,
            Some("http://127.0.0.1:7320"),
            Some("https://other.example"),
        )
        .unwrap();
        assert_eq!(
            parse(&explicit.to_string())
                .unwrap()
                .enterprise
                .unwrap()
                .center_url,
            "http://127.0.0.1:7320"
        );
        assert_eq!(
            parse(&personal_document().to_string()).unwrap().mode,
            HostMode::Personal
        );
    }

    #[test]
    fn invalid_or_missing_configuration_never_uses_a_new_package_default() {
        for text in [
            "version='0.27.0'",
            "version='0.27.0'\nmode='enterprise'",
            "version='0.27.0'\nmode='enterprise'\n[enterprise]\ncenter_url='https://user:secret@example.com'",
        ] {
            let mut doc = text.parse::<DocumentMut>().unwrap();
            assert!(
                set_mode_with_default(
                    &mut doc,
                    HostMode::Enterprise,
                    None,
                    Some("https://valid.example")
                )
                .is_err()
            );
        }
        for url in [
            "",
            "file:///tmp/center",
            "https://example.com?token=secret",
            "https://example.com/#x",
            "https://user@example.com",
            "https://example.com\nINJECT=value",
        ] {
            assert!(center_url::validate(url).is_err());
        }
    }
}
