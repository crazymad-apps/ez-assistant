import { AppleOutlined, WindowsOutlined } from '@ant-design/icons';
export const platforms = [
  {
    id: 'darwin-aarch64',
    name: 'macOS',
    chip: 'Apple Silicon',
    format: 'DMG',
    icon: AppleOutlined,
    hint: '适用于搭载 Apple 芯片的 Mac。',
    button: '下载 macOS 版',
  },
  {
    id: 'windows-x86_64',
    name: 'Windows',
    chip: 'x64',
    format: 'MSI',
    icon: WindowsOutlined,
    hint: '适用于搭载 Intel 或 AMD x64 处理器的电脑。',
    button: '下载 Windows x64 版',
  },
  {
    id: 'windows-aarch64',
    name: 'Windows',
    chip: 'ARM64',
    format: 'MSI',
    icon: WindowsOutlined,
    hint: '适用于搭载 ARM64 处理器的 Windows 电脑。',
    button: '下载 Windows ARM64 版',
  },
] as const;
