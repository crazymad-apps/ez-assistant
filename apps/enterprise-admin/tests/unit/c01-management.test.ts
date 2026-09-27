import { afterEach, describe, expect, it, vi } from 'vitest';
import { agentNameError, capabilityChoices, capabilityNameValid } from '../../src/features/agents/model';
import { versionCompare } from '../../src/features/plugins/model';
import { downloadPackage } from '../../src/features/plugins/download';
import { pluginApi } from '../../src/request/plugins';
import type { PluginRecord, PluginVersionSummary } from '../../src/request/openapi';

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('Agent 配置来源与输入', () => {
  it('中文名称、NFC 和 UTF-8 能力名在页面预检中与中心边界一致', () => {
    expect(agentNameError('文档审阅助手')).toBeUndefined();
    expect(agentNameError('e\u0301')).toBeDefined();
    expect(agentNameError(' 文档')).toBeDefined();
    expect(capabilityNameValid('skills', 'document-review')).toBe(true);
    expect(capabilityNameValid('mcp', 'local.server')).toBe(true);
    expect(capabilityNameValid('skills', 'BadName')).toBe(false);
  });

  it('能力候选按当前插件各包清单归并，不因同名其他插件而混入', () => {
    const plugins = [
      { name: 'one', versions: [{ skills: ['a', 'b'] }, { skills: ['b', 'c'] }] },
      { name: 'two', versions: [{ skills: ['other'] }] },
    ] as PluginRecord[];
    expect(capabilityChoices(plugins, 'one', 'skills')).toEqual(['a', 'b', 'c']);
    expect(capabilityChoices(plugins, 'absent', 'skills')).toEqual([]);
    expect(versionCompare('1.10.0', '1.9.0')).toBeLessThan(0);
  });
});

describe('受保护原包下载', () => {
  const version = {
    version: '1.0.0',
    archive_format: 'zip',
    sha256: 'a'.repeat(64),
    size_bytes: 3,
  } as PluginVersionSummary;

  it('文件选择后携带当前 SHA，逐块写入并核对长度才关闭文件', async () => {
    const writer = {
      write: vi.fn().mockResolvedValue(undefined),
      close: vi.fn().mockResolvedValue(undefined),
      abort: vi.fn().mockResolvedValue(undefined),
    };
    const picker = vi.fn().mockResolvedValue({ createWritable: async () => writer });
    vi.stubGlobal('window', { showSaveFilePicker: picker });
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new Uint8Array([1, 2, 3]));
        controller.close();
      },
    });
    const call = vi.spyOn(pluginApi, 'download').mockResolvedValue({
      headers: { 'content-length': '3' },
      data: stream,
    } as never);
    const progress = vi.fn();
    expect(await downloadPackage('token', 'document-tools', version, new AbortController().signal, progress)).toBe(
      'saved',
    );
    expect(call).toHaveBeenCalledWith(
      'token',
      'document-tools',
      '1.0.0',
      version.sha256,
      expect.any(AbortSignal),
      true,
    );
    expect(writer.write).toHaveBeenCalledOnce();
    expect(writer.close).toHaveBeenCalledOnce();
    expect(writer.abort).not.toHaveBeenCalled();
    expect(progress).toHaveBeenCalledWith(3, 3);
  });

  it('响应长度与详情不一致时中止目标文件，不宣称下载成功', async () => {
    const writer = { write: vi.fn(), close: vi.fn(), abort: vi.fn().mockResolvedValue(undefined) };
    vi.stubGlobal('window', { showSaveFilePicker: async () => ({ createWritable: async () => writer }) });
    vi.spyOn(pluginApi, 'download').mockResolvedValue({
      headers: { 'content-length': '4' },
      data: new Blob(),
    } as never);
    await expect(
      downloadPackage('token', 'document-tools', version, new AbortController().signal, vi.fn()),
    ).rejects.toThrow('长度');
    expect(writer.abort).toHaveBeenCalledOnce();
    expect(writer.close).not.toHaveBeenCalled();
  });

  it('无流式保存能力时，大包在网络请求前明确拒绝', async () => {
    vi.stubGlobal('window', {});
    const call = vi.spyOn(pluginApi, 'download');
    await expect(
      downloadPackage(
        'token',
        'document-tools',
        { ...version, size_bytes: 128 * 1024 * 1024 + 1 },
        new AbortController().signal,
        vi.fn(),
      ),
    ).rejects.toThrow('无法保存此大小');
    expect(call).not.toHaveBeenCalled();
  });
});
