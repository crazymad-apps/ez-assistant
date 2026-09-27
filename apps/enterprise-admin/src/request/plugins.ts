import * as sdk from './openapi/sdk.gen';
import type { PluginDescriptionInput, PackageUploadResult } from './openapi';

const options = (token: string, signal?: AbortSignal) => ({
  headers: { Authorization: `Bearer ${token}` },
  signal,
  throwOnError: true as const,
});

/** 包上传只传原 File；进度是请求字节，不等于中心校验或数据库发布完成。 */
export const pluginApi = {
  list: async (token: string, signal: AbortSignal) => (await sdk.listPlugins(options(token, signal))).data,

  get: async (token: string, name: string, signal?: AbortSignal) => {
    const response = await sdk.getPlugin({ ...options(token, signal), path: { name } });
    const etag: unknown = response.headers['etag'];
    return { detail: response.data, etag: typeof etag === 'string' ? etag : undefined };
  },

  edit: async (token: string, name: string, body: PluginDescriptionInput, etag: string) =>
    (
      await sdk.updatePlugin({
        ...options(token),
        path: { name },
        body,
        headers: { ...options(token).headers, 'If-Match': etag },
      })
    ).data,

  publication: async (token: string, name: string, listed: boolean, etag: string) =>
    (
      await sdk.setPluginPublication({
        ...options(token),
        path: { name },
        body: { listed },
        headers: { ...options(token).headers, 'If-Match': etag },
      })
    ).data,

  upload: async (
    token: string,
    file: File,
    signal: AbortSignal,
    progress: (sent: number, total: number) => void,
  ): Promise<PackageUploadResult> => {
    const format = file.name.toLowerCase().endsWith('.tar.gz') ? 'application/gzip' : 'application/zip';
    return (
      await sdk.uploadPluginPackage({
        ...options(token, signal),
        body: file,
        headers: { ...options(token).headers, 'Content-Type': format },
        timeout: 36 * 60 * 1000,
        onUploadProgress: (event) => progress(event.loaded, event.total ?? file.size),
      })
    ).data;
  },

  current: async (token: string, name: string, version: string, signal?: AbortSignal) =>
    (await sdk.getCurrentPluginPackage({ ...options(token, signal), path: { name, version } })).data,

  download: (token: string, name: string, version: string, sha: string, signal: AbortSignal, stream: boolean) =>
    sdk.downloadRuntimePluginPackage({
      ...options(token, signal),
      path: { name, version },
      headers: { ...options(token).headers, 'If-Match': `"${sha}"` },
      adapter: stream ? 'fetch' : undefined,
      responseType: stream ? 'stream' : 'blob',
      timeout: 35 * 60 * 1000,
    }),
};
