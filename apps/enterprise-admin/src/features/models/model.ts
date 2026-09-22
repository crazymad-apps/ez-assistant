import type { CatalogSnapshot, ModelConfiguration, ModelParameters, ProviderConnection } from '../../request/openapi';
export const providerLabels: Record<ProviderConnection['provider_type'], string> = {
  openai: 'OpenAI',
  deepseek: 'DeepSeek',
  dashscope_api: '百炼 API',
  dashscope_plan: '百炼套餐',
  moonshot: 'Moonshot / Kimi',
  zhipu: '智谱',
  vllm: 'vLLM',
  local: '本地兼容服务',
};
export const reasons: Record<string, string> = {
  not_selected: '尚未选择默认模型',
  provider_deleted: '默认服务商已删除',
  model_absent: '默认模型不在目录且无固定配置',
  invalid_parameters: '模型参数不完整或无效',
};
export const supports = [
  { value: 'unknown', label: '未知' },
  { value: 'supported', label: '支持' },
  { value: 'unsupported', label: '不支持' },
];
export const effortKeys = ['low', 'medium', 'high', 'xhigh', 'max'] as const;

export const modelPath = (id: string, model: string) =>
  `/models/providers/${id}/models/edit?${new URLSearchParams({ model_id: model })}`;

export function modelRows(catalog: CatalogSnapshot, fixed: readonly ModelConfiguration[]) {
  const map = new Map(fixed.map((row) => [row.model_id, row]));
  const rows = catalog.models.map((model) => {
    const saved = map.get(model.model_id);
    map.delete(model.model_id);
    return {
      id: model.model_id,
      name: model.display_name,
      origin: saved?.origin ?? 'online',
      fixed: Boolean(saved),
      online: true,
      template: !saved && Boolean(model.configuration?.uses_template),
      requires: saved?.requires_configuration ?? model.configuration?.requires_configuration ?? true,
    };
  });
  for (const row of map.values())
    rows.push({
      id: row.model_id,
      name: null,
      origin: row.origin,
      fixed: true,
      online: false,
      template: false,
      requires: row.requires_configuration,
    });
  return rows;
}

export const tokenFields = [
  ['context_window_tokens', '上下文窗口'],
  ['max_input_tokens', '最大输入'],
  ['max_output_tokens', '最大输出'],
  ['reasoning_max_input_tokens', '思考模式最大输入'],
  ['reasoning_max_output_tokens', '思考模式最大输出'],
] as const satisfies readonly (readonly [keyof ModelParameters, string])[];
