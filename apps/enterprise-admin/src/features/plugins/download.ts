import { pluginApi } from '../../request/plugins';
import type { PluginVersionSummary } from '../../request/openapi';

type SaveWriter = { write(bytes: Uint8Array): Promise<void>; close(): Promise<void>; abort(): Promise<void> };
type SaveHandle = { createWritable(): Promise<SaveWriter> };
type PickerWindow = Window & { showSaveFilePicker?: (options: { suggestedName: string }) => Promise<SaveHandle> };

/** 用户手势中先选择文件；原包响应只按当前 SHA 准入，逐块写入且核对长度。 */
export async function downloadPackage(
  token: string,
  name: string,
  version: PluginVersionSummary,
  signal: AbortSignal,
  progress: (loaded: number, total: number) => void,
): Promise<'saved' | 'browser'> {
  const extension = version.archive_format === 'zip' ? 'zip' : 'tar.gz';
  const filename = `${name}-${version.version}.${extension}`;
  const picker = (window as PickerWindow).showSaveFilePicker?.bind(window);
  if (!picker && version.size_bytes > 128 * 1024 * 1024)
    throw new Error('当前浏览器无法保存此大小的文件，请使用支持文件保存的浏览器并通过 HTTPS 或本机地址访问。');

  const handle = picker ? await picker({ suggestedName: filename }) : undefined;
  const writer = handle ? await handle.createWritable() : undefined;
  const idle = new AbortController();
  const combined = AbortSignal.any([signal, idle.signal]);
  let timer: ReturnType<typeof setTimeout> | undefined;
  const resetIdle = () => {
    clearTimeout(timer);
    timer = setTimeout(() => idle.abort(), 60_000);
  };
  resetIdle();
  try {
    const response = await pluginApi.download(token, name, version.version, version.sha256, combined, Boolean(writer));
    const length: unknown = response.headers['content-length'];
    const declared = typeof length === 'string' || typeof length === 'number' ? Number(length) : NaN;
    if (!Number.isSafeInteger(declared) || declared !== version.size_bytes)
      throw new Error('中心返回的包长度与详情不一致，请刷新后重试。');
    if (writer) {
      const stream: unknown = response.data;
      if (!(stream instanceof ReadableStream)) throw new Error('当前浏览器未提供可读的原包流。');
      const reader = stream.getReader();
      let loaded = 0;
      while (true) {
        const part = await reader.read();
        if (part.done) break;
        loaded += part.value.byteLength;
        if (loaded > declared) throw new Error('下载字节超过中心声明长度。');
        await writer.write(part.value);
        progress(loaded, declared);
        resetIdle();
      }
      if (loaded !== declared) throw new Error('下载字节不完整，请重新下载。');
      await writer.close();
      return 'saved';
    }
    const blob: unknown = response.data;
    if (!(blob instanceof Blob) || blob.size !== declared || blob.size > 128 * 1024 * 1024)
      throw new Error('浏览器取得的包长度不符或超出降级保存上限。');
    progress(blob.size, declared);
    const objectUrl = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = objectUrl;
    anchor.download = filename;
    anchor.click();
    setTimeout(() => URL.revokeObjectURL(objectUrl), 60_000);
    return 'browser';
  } catch (error) {
    await writer?.abort().catch(() => undefined);
    throw error;
  } finally {
    clearTimeout(timer);
  }
}
