import { createClient } from '../request/openapi/client';
import { getSiteDownloads, getSiteChangelog } from '../request/openapi';
import type { SiteContent, SiteDownload, SiteChangelogEntry } from '../request/openapi';

export const previewMode = import.meta.env.VITE_SITE_CONTENT_PREVIEW === 'true';
// 独立匿名客户端：不装配 AdminStore、Cookie 恢复或后台拦截器。
const client = createClient({ adapter: 'fetch', withCredentials: false, timeout: 15000 });

export async function readContent(signal: AbortSignal): Promise<SiteContent | null> {
  const response = await fetch(previewMode ? '/__content' : '/site-content/content.json', {
    signal: AbortSignal.any([signal, AbortSignal.timeout(15000)]),
    credentials: 'omit',
    cache: 'no-cache',
  });
  if (response.status === 404) return null;
  if (!response.ok) throw new Error('内容暂时无法读取，请重试。');
  const content: SiteContent = await response.json();
  if (
    content.format !== 1 ||
    !Array.isArray(content.home) ||
    !Array.isArray(content.manual) ||
    !Array.isArray(content.assets)
  )
    throw new Error('内容格式不受支持。');
  return content;
}

export async function readChangelog(signal: AbortSignal): Promise<SiteChangelogEntry[]> {
  if (previewMode) return [];
  return (await getSiteChangelog({ client, signal, throwOnError: true })).data;
}

export async function readDownloads(version: string | undefined, signal: AbortSignal): Promise<SiteDownload[]> {
  if (previewMode) return [];
  return (await getSiteDownloads({ client, query: version ? { version } : {}, signal, throwOnError: true })).data;
}

export function contentImages(content: SiteContent): Record<string, string> {
  return Object.fromEntries(
    content.assets
      .filter(
        (asset) =>
          /^[0-9a-f]{64}$/.test(asset.sha256) && ['image/png', 'image/jpeg', 'image/webp'].includes(asset.type),
      )
      .map((asset) => [
        asset.id,
        `${previewMode ? '/__assets' : '/site-content/assets'}/${asset.sha256}.${asset.type.slice(6)}`,
      ]),
  );
}
