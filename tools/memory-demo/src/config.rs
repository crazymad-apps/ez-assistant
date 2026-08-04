//! `chat` 子命令的 Provider 配置；credential 只保存在构造期私有值中。

use crate::DemoError;

pub(crate) struct ChatConfig {
    pub(crate) api_key: String,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) context_window_tokens: u64,
}

#[derive(Default)]
struct ChatConfigValues {
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    context_window_tokens: Option<String>,
}

pub(crate) fn load_chat_config() -> Result<ChatConfig, DemoError> {
    chat_config_from_values(ChatConfigValues {
        api_key: std::env::var("DEEPSEEK_API_KEY").ok(),
        base_url: std::env::var("DEEPSEEK_BASE_URL").ok(),
        model: std::env::var("DEEPSEEK_MODEL").ok(),
        context_window_tokens: std::env::var("DEEPSEEK_CONTEXT_WINDOW_TOKENS").ok(),
    })
}

fn chat_config_from_values(values: ChatConfigValues) -> Result<ChatConfig, DemoError> {
    let api_key = values
        .api_key
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            DemoError::Config(
                "missing DEEPSEEK_API_KEY; configure it in the repository .env".to_owned(),
            )
        })?;
    let base_url = values
        .base_url
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "https://api.deepseek.com".to_owned());
    let model = values
        .model
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "deepseek-v4-flash".to_owned());
    let context_window_tokens = values
        .context_window_tokens
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "128000".to_owned())
        .parse::<u64>()
        .map_err(|_| {
            DemoError::Config(
                "DEEPSEEK_CONTEXT_WINDOW_TOKENS must be a positive integer".to_owned(),
            )
        })?;
    if context_window_tokens == 0 {
        return Err(DemoError::Config(
            "DEEPSEEK_CONTEXT_WINDOW_TOKENS must be greater than zero".to_owned(),
        ));
    }
    Ok(ChatConfig {
        api_key,
        base_url,
        model,
        context_window_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_requires_credential_without_exposing_its_value() {
        let error = match chat_config_from_values(ChatConfigValues::default()) {
            Ok(_) => panic!("missing credential must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("DEEPSEEK_API_KEY"));

        let secret = "do-not-print-this-secret";
        let config = chat_config_from_values(ChatConfigValues {
            api_key: Some(secret.to_owned()),
            ..ChatConfigValues::default()
        })
        .expect("valid configuration");
        assert_eq!(config.base_url, "https://api.deepseek.com");
        assert_eq!(config.context_window_tokens, 128_000);
        assert!(!format!("{} {}", config.base_url, config.model).contains(secret));
    }

    #[test]
    fn config_rejects_invalid_context_window() {
        for value in ["0", "invalid"] {
            assert!(
                chat_config_from_values(ChatConfigValues {
                    api_key: Some("secret".to_owned()),
                    context_window_tokens: Some(value.to_owned()),
                    ..ChatConfigValues::default()
                })
                .is_err()
            );
        }
    }
}
