import { Modal } from 'antd';
import { DownloadOptions } from '../DownloadOptions';
import logo from '../assets/app-icon.png';
import styles from './index.module.scss';

export function DownloadDialog({ open, onClose }: { readonly open: boolean; readonly onClose: () => void }) {
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
      destroyOnHidden
    >
      {open && <DownloadOptions onNavigate={onClose} />}
    </Modal>
  );
}
