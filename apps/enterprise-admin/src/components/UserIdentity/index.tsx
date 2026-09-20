import { Avatar } from 'antd';
import type { UserView } from '../../model';
import styles from './index.module.scss';

export function UserIdentity({ user }: { readonly user: UserView }) {
  return (
    <div className={styles.identity}>
      <Avatar shape="square" className={styles.avatar}>
        {user.displayName.slice(0, 1)}
      </Avatar>
      <span className={styles.names}>
        <strong>{user.displayName}</strong>
        <span>{user.username}</span>
      </span>
    </div>
  );
}
