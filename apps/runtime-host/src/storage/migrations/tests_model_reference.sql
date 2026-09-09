-- M1 隔离结构夹具：只验证增量列、旧列删除与固定记录外键/数值约束；不是生产版本 SQL。
ALTER TABLE sessions ADD COLUMN model_provider_instance_id TEXT;
ALTER TABLE sessions ADD COLUMN model_id TEXT CHECK (
    (model_provider_instance_id IS NULL AND model_id IS NULL)
    OR (model_provider_instance_id IS NOT NULL AND length(model_provider_instance_id)>0
        AND model_id IS NOT NULL AND length(model_id)>0)
);
ALTER TABLE sessions DROP COLUMN model_key;
CREATE TABLE providers (provider_instance_id TEXT PRIMARY KEY NOT NULL);
CREATE TABLE model_fixed_configs (
    provider_instance_id TEXT NOT NULL REFERENCES providers(provider_instance_id) ON DELETE CASCADE,
    model_id TEXT NOT NULL CHECK(length(model_id)>0),
    context_window_tokens INTEGER NOT NULL CHECK(typeof(context_window_tokens)='integer' AND context_window_tokens BETWEEN 1 AND 9007199254740991),
    max_output_tokens INTEGER NOT NULL CHECK(typeof(max_output_tokens)='integer' AND max_output_tokens BETWEEN 1 AND 4294967295 AND max_output_tokens<=context_window_tokens),
    PRIMARY KEY(provider_instance_id, model_id)
);
