import * as sdk from './openapi/sdk.gen';
import type { AgentConfig } from './openapi';

const options = (token: string, signal?: AbortSignal) => ({
  headers: { Authorization: `Bearer ${token}` },
  signal,
  throwOnError: true as const,
});

/** ETag 取自本次详情响应；页面不得用列表时间或本地草稿推断条件写标识。 */
export const agentApi = {
  list: async (token: string, signal: AbortSignal) => (await sdk.listAgents(options(token, signal))).data,

  get: async (token: string, name: string, signal?: AbortSignal) => {
    const response = await sdk.getAgent({ ...options(token, signal), path: { name } });
    const etag: unknown = response.headers['etag'];
    return { detail: response.data, etag: typeof etag === 'string' ? etag : undefined };
  },

  create: async (token: string, body: AgentConfig) => (await sdk.createAgent({ ...options(token), body })).data,

  update: async (token: string, name: string, body: AgentConfig, etag: string) =>
    (
      await sdk.updateAgent({
        ...options(token),
        path: { name },
        body,
        headers: { ...options(token).headers, 'If-Match': etag },
      })
    ).data,

  remove: async (token: string, name: string, etag: string) => {
    await sdk.deleteAgent({
      ...options(token),
      path: { name },
      headers: { ...options(token).headers, 'If-Match': etag },
    });
  },
};
