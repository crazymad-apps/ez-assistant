import { describe, expect, it } from 'vitest';
import { detectDownloadPlatform } from '../../src/site/DownloadOptions/useDownloadPlatform';

const desktop = { platform: 'Win32', userAgent: 'Windows NT 10.0; Win64; x64', maxTouchPoints: 0 };

describe('官网安装包设备匹配', () => {
  it.each([
    ['Windows', 'x86', 'windows-x86_64'],
    ['Windows', 'arm', 'windows-aarch64'],
    ['macOS', 'arm', 'darwin-aarch64'],
  ])('按系统和明确的 64 位芯片提示匹配 %s / %s', (platform, architecture, target) => {
    expect(detectDownloadPlatform(desktop, { platform, architecture, bitness: '64' }).target).toBe(target);
  });

  it('不把 Windows ARM 浏览器的 x64 兼容 UA 当作真实芯片', () => {
    expect(detectDownloadPlatform(desktop).target).toBe('');
    expect(detectDownloadPlatform(desktop).label).toBe('下载 Windows 版');
  });

  it('缺少或不支持的芯片提示不能匹配其他安装包', () => {
    for (const hints of [
      { platform: 'Windows', architecture: 'x86', bitness: '32' },
      { platform: 'Windows', architecture: 'arm' },
      { platform: 'Linux', architecture: 'x86', bitness: '64' },
    ])
      expect(detectDownloadPlatform(desktop, hints).target).toBe('');
  });

  it('识别为 macOS 时直接默认选中 Apple Silicon，不要求浏览器提供芯片信息', () => {
    const result = detectDownloadPlatform({ ...desktop, platform: 'MacIntel', userAgent: 'Macintosh; Intel Mac OS X' });
    expect(result.target).toBe('darwin-aarch64');
    expect(result.label).toBe('下载 macOS 版');
    expect(detectDownloadPlatform(desktop, { platform: 'macOS', architecture: 'x86', bitness: '64' }).target).toBe(
      'darwin-aarch64',
    );
  });

  it('iPad 桌面模式和手机不自动匹配桌面包', () => {
    expect(detectDownloadPlatform({ platform: 'MacIntel', userAgent: 'Macintosh', maxTouchPoints: 5 }).target).toBe('');
    expect(
      detectDownloadPlatform(
        { ...desktop, userAgent: 'Android' },
        { platform: 'Windows', architecture: 'arm', bitness: '64' },
      ).target,
    ).toBe('');
  });
});
