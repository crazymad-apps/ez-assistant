import * as sdk from './openapi/sdk.gen';
import type {
  ModelConfiguration,
  ModelConfigurationQuery,
  ModelSelection,
  SaveModelConfiguration,
  SaveProviderWritable as SaveProvider,
} from './openapi';

const options = (token: string, signal?: AbortSignal) => ({
  headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
  signal,
  throwOnError: true as const,
});

// 模型 feature 使用生成契约；显式携带发起时身份，禁止自行拼接 URL 或二次定义 DTO。
export const modelApi = {
  providers: async (token: string, signal?: AbortSignal) => (await sdk.listModelProviders(options(token, signal))).data,

  provider: async (token: string, id: string, signal?: AbortSignal) =>
    (await sdk.getModelProvider({ ...options(token, signal), path: { id } })).data,

  saveProvider: async (token: string, body: SaveProvider, id?: string) => {
    if (id) return (await sdk.updateModelProvider({ ...options(token), path: { id }, body })).data;
    return (await sdk.createModelProvider({ ...options(token), body })).data;
  },

  usage: async (token: string, id: string, signal?: AbortSignal) =>
    (await sdk.getModelProviderUsage({ ...options(token, signal), path: { id } })).data,

  remove: async (token: string, id: string) =>
    (await sdk.deleteModelProvider({ ...options(token), path: { id } })).data,

  catalog: async (token: string, id: string, signal?: AbortSignal) =>
    (await sdk.getModelCatalog({ ...options(token, signal), path: { id } })).data,

  refresh: async (token: string, id: string) =>
    (await sdk.refreshModelCatalog({ ...options(token), path: { id }, body: {} })).data,

  fixed: async (token: string, id: string, signal?: AbortSignal) => {
    const items: ModelConfiguration[] = [];
    let offset = 0;
    while (true) {
      const page = (
        await sdk.listFixedModelConfigurations({
          ...options(token, signal),
          path: { id },
          query: { offset, limit: 100 },
        })
      ).data;
      items.push(...page.items);
      offset += page.items.length;
      if (!page.items.length || offset >= page.total) return items;
    }
  },

  configuration: async (token: string, body: ModelConfigurationQuery, signal?: AbortSignal) =>
    (await sdk.queryModelConfiguration({ ...options(token, signal), body })).data,

  save: async (token: string, body: SaveModelConfiguration) =>
    (await sdk.saveModelConfiguration({ ...options(token), body })).data,

  reset: async (token: string, body: ModelSelection) => sdk.resetModelConfiguration({ ...options(token), body }),

  settings: async (token: string, signal?: AbortSignal) => (await sdk.getModelSettings(options(token, signal))).data,

  setDefault: async (token: string, default_model: ModelSelection | null) =>
    (await sdk.saveModelSettings({ ...options(token), body: { default_model } })).data,

  templates: async (token: string, signal?: AbortSignal) =>
    (await sdk.getModelTemplateStatus(options(token, signal))).data,

  reload: async (token: string) => (await sdk.reloadModelTemplates({ ...options(token), body: {} })).data,
};
