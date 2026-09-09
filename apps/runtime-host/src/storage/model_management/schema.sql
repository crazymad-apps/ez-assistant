CREATE TABLE providers (
    provider_instance_id TEXT PRIMARY KEY NOT NULL CHECK(length(provider_instance_id) > 0),
    display_name TEXT NOT NULL CHECK(length(display_name) > 0),
    provider_type TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    api_key TEXT NOT NULL,
    protocol_preference TEXT NOT NULL,
    models_path TEXT NOT NULL,
    discovery_format TEXT NOT NULL
);
CREATE TABLE model_settings (
    singleton_key INTEGER PRIMARY KEY CHECK(singleton_key = 1),
    default_provider_instance_id TEXT,
    default_model_id TEXT,
    vision_provider_instance_id TEXT,
    vision_model_id TEXT,
    CHECK ((default_provider_instance_id IS NULL AND default_model_id IS NULL) OR
        (default_provider_instance_id IS NOT NULL AND length(default_provider_instance_id) > 0 AND default_model_id IS NOT NULL AND length(default_model_id) > 0)),
    CHECK ((vision_provider_instance_id IS NULL AND vision_model_id IS NULL) OR
        (vision_provider_instance_id IS NOT NULL AND length(vision_provider_instance_id) > 0 AND vision_model_id IS NOT NULL AND length(vision_model_id) > 0))
);
INSERT INTO model_settings(singleton_key) VALUES (1);
CREATE TABLE model_fixed_configs (
    origin TEXT NOT NULL DEFAULT 'online' CHECK(origin IN ('online','manual')),
    provider_instance_id TEXT NOT NULL REFERENCES providers(provider_instance_id) ON DELETE CASCADE,
    model_id TEXT NOT NULL CHECK(length(model_id) > 0),
    context_window_tokens INTEGER NOT NULL CHECK(typeof(context_window_tokens) = 'integer' AND context_window_tokens BETWEEN 1 AND 9007199254740991),
    max_output_tokens INTEGER NOT NULL CHECK(typeof(max_output_tokens) = 'integer' AND max_output_tokens BETWEEN 1 AND 4294967295 AND max_output_tokens <= context_window_tokens),
    max_input_tokens INTEGER CHECK(max_input_tokens IS NULL OR (typeof(max_input_tokens) = 'integer' AND max_input_tokens BETWEEN 1 AND context_window_tokens)),
    mode_limits_json TEXT NOT NULL CHECK(json_valid(mode_limits_json) AND json_type(mode_limits_json) = 'object'),
    capabilities_json TEXT NOT NULL CHECK(json_valid(capabilities_json) AND json_type(capabilities_json) = 'object'),
    updated_at_ms INTEGER NOT NULL CHECK(typeof(updated_at_ms) = 'integer' AND updated_at_ms >= 0),
    PRIMARY KEY(provider_instance_id, model_id)
);
