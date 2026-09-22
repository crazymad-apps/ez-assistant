import * as sdk from './openapi/sdk.gen';
import type { ListLlmCallsData, RecordingSettings, ModelSelection } from './openapi';

const options = (token: string, signal?: AbortSignal) => ({
  headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
  signal,
  throwOnError: true as const,
});

export const callApi = {
  list: async (token: string, query: ListLlmCallsData['query'], signal?: AbortSignal) =>
    (await sdk.listLlmCalls({ ...options(token, signal), query })).data,

  get: async (token: string, id: string, signal?: AbortSignal) =>
    (await sdk.getLlmCall({ ...options(token, signal), path: { id } })).data,

  snapshot: async (token: string, id: string, side: 'request' | 'response', signal?: AbortSignal) =>
    (await sdk.getLlmSnapshot({ ...options(token, signal), path: { id }, query: { side } })).data,

  settings: async (token: string, signal?: AbortSignal) =>
    (await sdk.getLlmRecordingSettings(options(token, signal))).data,

  save: async (token: string, body: RecordingSettings) =>
    (await sdk.saveLlmRecordingSettings({ ...options(token), body })).data,

  test: async (token: string, body: ModelSelection, signal: AbortSignal) =>
    (await sdk.testModelConfiguration({ ...options(token, signal), body, timeout: 35000 })).data,
};
