import { Modal } from 'antd';
import styles from './index.module.scss';

type InfoDialogProps = { readonly kind: 'admin' | null; readonly onClose: () => void };

export function InfoDialog({ kind, onClose }: InfoDialogProps) {
  return (
    <Modal open={kind !== null} title="企业管理后台" onCancel={onClose} footer={null} width={680}>
      <div className={styles.body}>
        <p>正式官网将在这里进入现有企业管理后台。</p>
        <p className={styles.article}>首页保持免登录访问；安装包、模型和账号等管理操作继续要求管理员身份。</p>
        <p className={styles.note}>当前 demo 不连接真实后台，也不会收集登录信息。</p>
      </div>
    </Modal>
  );
}
