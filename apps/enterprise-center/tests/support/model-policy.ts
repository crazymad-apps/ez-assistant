import type { ModelSelection, ModelSettings, RuntimeModelConfiguration } from '../../dist/contracts/models.js';

export function singleModelPolicy(settings: ModelSettings, selected: ModelSelection | null) {
  return {
    policy: {
      revision: settings.policy?.revision ?? null,
      default_model: selected,
      allowed_models: selected ? [selected] : [],
      allow_member_selection: false,
    },
  };
}

export function defaultRuntimeModel(configuration: RuntimeModelConfiguration) {
  const selected = configuration.default_model;
  const provider = configuration.providers.find((item) => item.provider_instance_id === selected?.provider_instance_id);
  const model = provider?.models.find((item) => item.model_id === selected?.model_id);
  if (!provider || !model) throw new Error('Missing runtime default model');
  return { ...provider, ...model };
}

export function defaultHistoryModel(configuration: RuntimeModelConfiguration) {
  const model = defaultRuntimeModel(configuration);
  if (!model.server_history_binding) throw new Error('Missing runtime default history binding');
  return { ...model, server_history_binding: model.server_history_binding };
}
