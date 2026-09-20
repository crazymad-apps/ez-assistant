import { Avatar } from 'antd';
import type { DemoUser } from '../../model';
import styles from './index.module.scss';

export function UserIdentity({ user }: { readonly user: DemoUser }) {
  return <div className={styles.identity}>
    <Avatar shape="square" className={styles.avatar}>{user.displayName.slice(0, 1)}</Avatar>
    <span className={styles.names}><strong>{user.displayName}</strong><span>{user.username}</span></span>
  </div>;
}
