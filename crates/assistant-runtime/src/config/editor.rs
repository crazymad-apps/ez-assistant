//! `config.toml` 中模型配置的保结构编辑。

use assistant_protocol::{ModelConfigurationInput, ModelCredentialChange, ModelKey};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::{RuntimeError, RuntimeResult};

pub(crate) enum ConfigMutation {
    Create {
        model: ModelConfigurationInput,
        set_default: bool,
    },
    Update {
        model: ModelConfigurationInput,
        set_default: bool,
    },
    Delete {
        model_key: ModelKey,
        replacement_default: Option<ModelKey>,
    },
    SetDefault {
        model_key: ModelKey,
    },
    SetAuxiliaryVision {
        model_key: Option<ModelKey>,
    },
}

impl ConfigMutation {
    pub(crate) fn target_model_key(&self) -> Option<&ModelKey> {
        match self {
            Self::Create { model, .. } | Self::Update { model, .. } => Some(&model.model_key),
            Self::SetDefault { model_key } => Some(model_key),
            Self::SetAuxiliaryVision { model_key } => model_key.as_ref(),
            Self::Delete { .. } => None,
        }
    }

    pub(crate) fn requires_image_input(&self) -> bool {
        matches!(self, Self::SetAuxiliaryVision { model_key: Some(_) })
    }
}

pub(crate) fn edit_config_document(
    current: Option<&str>,
    mutation: ConfigMutation,
) -> RuntimeResult<String> {
    let mut document = match current {
        Some(current) => current
            .parse::<DocumentMut>()
            .map_err(|_| RuntimeError::ConfigurationUnavailable)?,
        None => {
            let mut document = DocumentMut::new();
            document["schema_version"] = value(1);
            document["default_model"] = value("");
            document["models"] = Item::Table(Table::new());
            document
        }
    };

    match mutation {
        ConfigMutation::Create { model, set_default } => {
            let key = model.model_key.as_str().to_owned();
            let models = models_table(&mut document)?;
            if models.contains_key(&key) {
                return Err(RuntimeError::InvalidRequest {
                    reason: "model key already exists",
                });
            }
            if matches!(model.credential, ModelCredentialChange::Unchanged) {
                return Err(RuntimeError::InvalidRequest {
                    reason: "new model credential is required",
                });
            }
            models.insert(&key, Item::Table(model_table(model, None)?));
            if set_default || document["default_model"].as_str().is_none_or(str::is_empty) {
                document["default_model"] = value(key);
            }
        }
        ConfigMutation::Update { model, set_default } => {
            let key = model.model_key.as_str().to_owned();
            let models = models_table(&mut document)?;
            let current = models
                .get_mut(&key)
                .and_then(Item::as_table_mut)
                .ok_or_else(|| RuntimeError::ModelNotFound {
                    model_key: model.model_key.clone(),
                })?;
            let existing_secret = current
                .get("api_key")
                .and_then(Item::as_str)
                .map(str::to_owned);
            apply_model_to_table(current, model, existing_secret.as_deref())?;
            if set_default {
                document["default_model"] = value(key);
            }
        }
        ConfigMutation::Delete {
            model_key,
            replacement_default,
        } => {
            let key = model_key.as_str();
            let default_is_target = document["default_model"].as_str() == Some(key);
            let models = models_table(&mut document)?;
            if models.remove(key).is_none() {
                return Err(RuntimeError::ModelNotFound { model_key });
            }
            if default_is_target {
                let replacement = replacement_default.ok_or(RuntimeError::InvalidRequest {
                    reason: "replacement default model is required",
                })?;
                if !models.contains_key(replacement.as_str()) {
                    return Err(RuntimeError::ModelNotFound {
                        model_key: replacement,
                    });
                }
                document["default_model"] = value(replacement.as_str());
            }
        }
        ConfigMutation::SetDefault { model_key } => {
            if !models_table(&mut document)?.contains_key(model_key.as_str()) {
                return Err(RuntimeError::ModelNotFound { model_key });
            }
            document["default_model"] = value(model_key.as_str());
        }
        ConfigMutation::SetAuxiliaryVision { model_key } => match model_key {
            Some(model_key) => {
                if !models_table(&mut document)?.contains_key(model_key.as_str()) {
                    return Err(RuntimeError::ModelNotFound { model_key });
                }
                let vision = vision_table(&mut document)?;
                vision["model_key"] = value(model_key.as_str());
                if vision.get("timeout_ms").is_none() {
                    vision["timeout_ms"] = value(60_000_i64);
                }
                if vision.get("max_output_tokens").is_none() {
                    vision["max_output_tokens"] = value(4_096_i64);
                }
            }
            None => {
                let mut remove_empty_agent = false;
                if let Some(agent) = document.get_mut("agent").and_then(Item::as_table_mut) {
                    agent.remove("vision");
                    remove_empty_agent = agent.is_empty();
                }
                if remove_empty_agent {
                    document.remove("agent");
                }
            }
        },
    }

    Ok(document.to_string())
}

fn vision_table(document: &mut DocumentMut) -> RuntimeResult<&mut Table> {
    if document.get("agent").is_none() {
        document["agent"] = Item::Table(Table::new());
    }
    let agent = document["agent"]
        .as_table_mut()
        .ok_or(RuntimeError::ConfigurationUnavailable)?;
    if agent.get("vision").is_none() {
        agent["vision"] = Item::Table(Table::new());
    }
    agent["vision"]
        .as_table_mut()
        .ok_or(RuntimeError::ConfigurationUnavailable)
}

fn models_table(document: &mut DocumentMut) -> RuntimeResult<&mut Table> {
    if document.get("models").is_none() {
        document["models"] = Item::Table(Table::new());
    }
    document["models"]
        .as_table_mut()
        .ok_or(RuntimeError::ConfigurationUnavailable)
}

fn model_table(
    model: ModelConfigurationInput,
    existing_secret: Option<&str>,
) -> RuntimeResult<Table> {
    let mut table = Table::new();
    apply_model_to_table(&mut table, model, existing_secret)?;
    Ok(table)
}

fn apply_model_to_table(
    table: &mut Table,
    model: ModelConfigurationInput,
    existing_secret: Option<&str>,
) -> RuntimeResult<()> {
    table["display_name"] = value(model.display_name);
    table["protocol"] = value(match model.protocol.as_str() {
        "chat_completions" => "openai_chat_completions".to_owned(),
        _ => model.protocol,
    });
    table["provider"] = value(model.provider);
    table["endpoint"] = value(model.endpoint);
    table["model"] = value(model.model);
    match model.credential {
        ModelCredentialChange::Unchanged => {
            let secret = existing_secret.ok_or(RuntimeError::InvalidRequest {
                reason: "existing model credential is unavailable",
            })?;
            table["api_key"] = value(secret);
        }
        ModelCredentialChange::Replace(secret) => {
            table["api_key"] = value(secret.expose());
        }
        ModelCredentialChange::Clear => {
            table.remove("api_key");
        }
    }
    table["context_window_tokens"] = value(model.context_window_tokens as i64);
    table["max_output_tokens"] = value(i64::from(model.max_output_tokens));
    Ok(())
}

#[cfg(test)]
mod tests {
    use assistant_protocol::SecretValue;

    use super::*;

    fn model(credential: ModelCredentialChange) -> ModelConfigurationInput {
        ModelConfigurationInput {
            model_key: ModelKey::new("fixture").expect("key"),
            display_name: "Fixture".to_owned(),
            protocol: "chat_completions".to_owned(),
            provider: "fixture".to_owned(),
            endpoint: "https://api.example.test/v1".to_owned(),
            model: "fixture-model".to_owned(),
            context_window_tokens: 8_192,
            max_output_tokens: 4_096,
            credential,
        }
    }

    #[test]
    fn creates_model_and_preserves_unrelated_comments() {
        let document = "# keep me\nschema_version = 1\ndefault_model = \"\"\n";
        let edited = edit_config_document(
            Some(document),
            ConfigMutation::Create {
                model: model(ModelCredentialChange::Replace(SecretValue::new(
                    "secret".to_owned(),
                ))),
                set_default: true,
            },
        )
        .expect("edit");
        assert!(edited.contains("# keep me"));
        assert!(edited.contains("[models.fixture]"));
        assert!(edited.contains("api_key = \"secret\""));
        assert!(edited.contains("protocol = \"openai_chat_completions\""));
    }

    #[test]
    fn creates_and_reloads_a_model_with_a_dotted_key() {
        let mut dotted = model(ModelCredentialChange::Replace(SecretValue::new(
            "secret".to_owned(),
        )));
        dotted.model_key = ModelKey::new("qwen3.8-max").expect("dotted key");
        let edited = edit_config_document(
            None,
            ConfigMutation::Create {
                model: dotted,
                set_default: true,
            },
        )
        .expect("create dotted model");

        let document = edited.parse::<DocumentMut>().expect("parse edited config");
        assert_eq!(document["default_model"].as_str(), Some("qwen3.8-max"));
        assert!(document["models"]["qwen3.8-max"].is_table());
    }

    #[test]
    fn unchanged_credential_survives_update() {
        let created = edit_config_document(
            None,
            ConfigMutation::Create {
                model: model(ModelCredentialChange::Replace(SecretValue::new(
                    "secret".to_owned(),
                ))),
                set_default: true,
            },
        )
        .expect("create");
        let updated = edit_config_document(
            Some(&created),
            ConfigMutation::Update {
                model: model(ModelCredentialChange::Unchanged),
                set_default: false,
            },
        )
        .expect("update");
        assert!(updated.contains("api_key = \"secret\""));
    }

    #[test]
    fn update_preserves_model_comments_and_key_order() {
        let document = r#"schema_version = 1
default_model = "fixture"

[models.fixture]
# keep model comment
display_name = "Old"
protocol = "chat_completions"
provider = "fixture"
endpoint = "https://api.example.test/v1"
model = "fixture-model"
api_key = "secret"
context_window_tokens = 8192
max_output_tokens = 4096
"#;
        let edited = edit_config_document(
            Some(document),
            ConfigMutation::Update {
                model: model(ModelCredentialChange::Unchanged),
                set_default: false,
            },
        )
        .expect("update");
        assert!(edited.contains("# keep model comment"));
        assert!(
            edited.find("display_name").expect("display name")
                < edited.find("protocol").expect("protocol")
        );
    }

    #[test]
    fn update_preserves_advanced_capability_override() {
        let document = r#"schema_version = 1
default_model = "fixture"

[models.fixture]
display_name = "Old"
protocol = "chat_completions"
provider = "fixture"
endpoint = "https://api.example.test/v1"
model = "fixture-model"
api_key = "secret"
context_window_tokens = 8192
max_output_tokens = 4096

[models.fixture.capabilities]
image_input = true
"#;
        let edited = edit_config_document(
            Some(document),
            ConfigMutation::Update {
                model: model(ModelCredentialChange::Unchanged),
                set_default: false,
            },
        )
        .expect("update");
        assert!(edited.contains("[models.fixture.capabilities]"));
        assert!(edited.contains("image_input = true"));
        assert!(edited.contains("protocol = \"openai_chat_completions\""));
    }

    #[test]
    fn sets_and_clears_auxiliary_vision_without_rewriting_agent_defaults() {
        let document = r#"schema_version = 1
default_model = "fixture"

[agent.defaults.execution_limits]
max_steps = 12

[models.fixture]
display_name = "Fixture"
protocol = "openai_chat_completions"
provider = "fixture"
endpoint = "https://api.example.test/v1"
model = "fixture-model"
api_key = "secret"
context_window_tokens = 8192
max_output_tokens = 4096

[models.fixture.capabilities]
image_input = true
"#;
        let selected = edit_config_document(
            Some(document),
            ConfigMutation::SetAuxiliaryVision {
                model_key: Some(ModelKey::new("fixture").expect("key")),
            },
        )
        .expect("select vision model");
        assert!(selected.contains("[agent.defaults.execution_limits]"));
        assert!(selected.contains("[agent.vision]"));
        assert!(selected.contains("model_key = \"fixture\""));
        assert!(selected.contains("timeout_ms = 60000"));
        assert!(selected.contains("max_output_tokens = 4096"));

        let cleared = edit_config_document(
            Some(&selected),
            ConfigMutation::SetAuxiliaryVision { model_key: None },
        )
        .expect("clear vision model");
        assert!(cleared.contains("[agent.defaults.execution_limits]"));
        assert!(!cleared.contains("[agent.vision]"));
    }
}
