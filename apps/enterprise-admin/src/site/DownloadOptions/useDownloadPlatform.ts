import { useEffect, useState } from 'react';
import { platforms } from './platforms';

type DeviceHints = { platform?: string; architecture?: string; bitness?: string; mobile?: boolean };
type DeviceNavigator = Pick<Navigator, 'platform' | 'userAgent' | 'maxTouchPoints'> & {
  userAgentData?: DeviceHints & { getHighEntropyValues?: (hints: string[]) => Promise<DeviceHints> };
};

/** macOS 默认推荐 Apple Silicon；Windows 仍需明确芯片提示，不能用兼容 UA 推断。 */
export function detectDownloadPlatform(device: DeviceNavigator, hints = device.userAgentData) {
  const platform = hints?.platform || device.platform || device.userAgent;
  const mobile = hints?.mobile || /Android|iPhone|iPad|iPod/i.test(device.userAgent);
  if (mobile || (/Mac/i.test(platform) && device.maxTouchPoints > 1))
    return { label: '下载桌面客户端', target: '', hint: '请在 Mac 或 Windows 电脑上安装，或手动选择需要的版本。' };
  const mac = /Mac/i.test(platform);
  const windows = /Win/i.test(platform);
  let target = mac ? 'darwin-aarch64' : '';
  if (hints?.bitness === '64') {
    if (windows && hints.architecture === 'arm') target = 'windows-aarch64';
    if (windows && hints.architecture === 'x86') target = 'windows-x86_64';
  }
  const selected = platforms.find((item) => item.id === target);
  if (selected)
    return {
      label: selected.button,
      target,
      hint: mac
        ? '已识别为 macOS，默认选择 Apple Silicon 版本，也可切换其他版本。'
        : `已根据当前设备选择 ${selected.name} · ${selected.chip}，也可切换其他版本。`,
    };
  if (windows)
    return { label: '下载 Windows 版', target: '', hint: '已识别为 Windows，请确认电脑使用 x64 还是 ARM64 芯片。' };
  return { label: '下载桌面客户端', target: '', hint: '请选择与你设备匹配的安装平台和芯片类型。' };
}

/** 仅在本地读取设备提示；不上传或持久化，缺少接口／被拒绝时保留系统级推荐。 */
export function useDownloadPlatform() {
  const [device, setDevice] = useState(() => detectDownloadPlatform(navigator));
  useEffect(() => {
    let active = true;
    const source = navigator as DeviceNavigator;
    const data = source.userAgentData;
    if (data?.getHighEntropyValues) {
      void data.getHighEntropyValues(['architecture', 'bitness']).then(
        (hints) => {
          if (active) setDevice(detectDownloadPlatform(source, { ...data, ...hints }));
        },
        () => {
          /* 浏览器未提供芯片信息时，由用户确认。 */
        },
      );
    }
    return () => {
      active = false;
    };
  }, []);
  return device;
}
