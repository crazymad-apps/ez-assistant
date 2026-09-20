import { Descriptions, Modal, Tag } from 'antd';
import { UserIdentity } from '../../../components/UserIdentity';
import type { DemoUser } from '../../../model';
import styles from './index.module.scss';

type ProfileDialogProps = Readonly<{ user: DemoUser; onClose: () => void }>;

/** 账号信息弹窗：从右上角用户菜单进入，替代独立页面。 */
export function ProfileDialog(props: ProfileDialogProps) {
  return <Modal open width={420} title="账号信息" footer={null} onCancel={props.onClose}>
    <div className={styles.profile}>
      <UserIdentity user={props.user} />
      <div className={styles.tags}>
        <Tag color="blue">{props.user.role === 'admin' ? '管理员' : '普通用户'}</Tag>
        {props.user.superAdmin && <Tag color="gold">超级管理员</Tag>}
      </div>
    </div>
    <Descriptions column={1} size="small" items={[
      { key: 'username', label: '登录账号', children: props.user.username },
      { key: 'status', label: '账号状态', children: '启用' },
      { key: 'date', label: '创建时间', children: props.user.createdAt },
    ]} />
  </Modal>;
}
