import type { DesktopTargetSummary } from '../../request/openapi';

export const platformLabels: Record<DesktopTargetSummary['target'], string> = {
  'windows-x86_64': 'Windows x64',
  'windows-aarch64': 'Windows arm64',
  'darwin-aarch64': 'macOS arm64',
};
export const publicationLabels = { draft: '草稿', published: '已发布', withdrawn: '已撤回' };
export const releasePath = (version: string) => `/desktop-releases/${encodeURIComponent(version)}`;

/** 本地只作选中文件的预览和匹配，信任判断始终由中心验签决定。 */
export async function selectRelease(files: File[]) {
  if (files.length < 3 || files.length > 4 || new Set(files.map((file) => file.name)).size !== files.length)
    throw new Error('请选择同一平台的清单、签名和安装文件。');
  const manifestFile = files.find((file) => file.name === 'release.json');
  const signatureFile = files.find((file) => file.name === 'release.json.sig');
  if (!manifestFile || !signatureFile || manifestFile.size > 65536 || signatureFile.size > 8192)
    throw new Error('缺少或无效的 release.json / release.json.sig。');
  const text = await manifestFile.text(),
    signature = await signatureFile.text();
  const value: unknown = JSON.parse(text);
  if (!value || typeof value !== 'object') throw new Error('发布清单格式不正确。');
  const version: unknown = Reflect.get(value, 'version'),
    target: unknown = Reflect.get(value, 'target'),
    entries: unknown = Reflect.get(value, 'files');
  if (
    typeof version !== 'string' ||
    !/^\d+\.\d+\.\d+$/.test(version) ||
    typeof target !== 'string' ||
    !Object.hasOwn(platformLabels, target) ||
    !Array.isArray(entries)
  )
    throw new Error('发布清单格式不正确。');
  const artifacts = files.filter((file) => file !== manifestFile && file !== signatureFile);
  if (
    entries.length !== artifacts.length ||
    entries.some(
      (entry: unknown) =>
        !entry ||
        typeof entry !== 'object' ||
        !artifacts.some(
          (file) =>
            file.name === Reflect.get(entry, 'name') &&
            file.size === Reflect.get(entry, 'size') &&
            file.size > 0 &&
            file.size <= 536870912,
        ),
    )
  )
    throw new Error('安装文件与清单不匹配。');
  // HTTP 管理后台也须可用；核对签名清单原文，不依赖仅安全上下文可用的 Web Crypto。
  return { text, signature, artifacts, version, target: target as DesktopTargetSummary['target'] };
}
