import { useState } from 'react';
import { Link } from 'react-router';
import { Button, Modal, Segmented } from 'antd';
import { AppleOutlined, WindowsOutlined, DownloadOutlined, ArrowRightOutlined } from '@ant-design/icons';
import logo from '../assets/app-icon.svg';
import styles from './index.module.scss';

type DownloadDialogProps = { readonly open: boolean; readonly onClose: () => void };
const platforms = [
  {
    id: 'mac',
    name: 'macOS',
    chip: 'Apple Silicon',
    format: 'DMG',
    icon: AppleOutlined,
    hint: '适用于搭载 Apple 芯片的 Mac。',
    button: '下载 macOS 版',
  },
  {
    id: 'win',
    name: 'Windows',
    chip: 'x64',
    format: 'MSI',
    icon: WindowsOutlined,
    hint: '适用于搭载 Intel 或 AMD x64 处理器的电脑。',
    button: '下载 Windows x64 版',
  },
  {
    id: 'arm',
    name: 'Windows',
    chip: 'ARM64',
    format: 'MSI',
    icon: WindowsOutlined,
    hint: '适用于搭载 ARM64 处理器的 Windows 电脑。',
    button: '下载 Windows ARM64 版',
  },
] as const;

export function DownloadDialog({ open, onClose }: DownloadDialogProps) {
  const [platform, setPlatform] = useState('mac');
  const [attempted, setAttempted] = useState(false);
  const selected = platforms.find((item) => item.id === platform) ?? platforms[0];

  return (
    <Modal
      title={
        <div className={styles.heading}>
          <img src={logo} alt="" />
          <div>
            <h2>下载 EZ Assistant</h2>
            <p>选择与你的系统和芯片匹配的版本。</p>
          </div>
        </div>
      }
      open={open}
      onCancel={onClose}
      footer={null}
      width={560}
      centered
    >
      <div className={styles.body}>
        <Segmented
          block
          className={styles.platforms}
          aria-label="安装平台与芯片类型"
          value={platform}
          onChange={(value) => {
            setPlatform(value);
            setAttempted(false);
          }}
          options={platforms.map((item) => ({
            value: item.id,
            label: (
              <span className={styles.platform_content}>
                <span className={styles.platform_name}>
                  <item.icon />
                  <strong>{item.name}</strong>
                </span>
                <small>{item.chip}</small>
              </span>
            ),
          }))}
        />
        <div className={styles.package}>
          <div className={styles.package_heading}>
            <strong>
              {selected.name} · {selected.chip}
            </strong>
            <span>{selected.format} 安装包</span>
          </div>
          <p>{selected.hint}</p>
          <Link className={styles.help} to="/manual/installation" onClick={onClose}>
            查看安装说明 <ArrowRightOutlined />
          </Link>
        </div>
        <Button type="primary" size="large" block icon={<DownloadOutlined />} onClick={() => setAttempted(true)}>
          {selected.button}
        </Button>
        <div className={styles.notice} role="status" aria-live="polite">
          <span>{attempted ? '下载交互已演示，当前不会下载真实文件。' : '演示界面，暂未提供真实安装包。'}</span>
        </div>
      </div>
    </Modal>
  );
}
