import { useLayoutEffect, useRef, useState } from 'react';
import { observer } from 'mobx-react-lite';
import { Avatar, Button, Dropdown } from 'antd';
import { useLeaveConfirmation } from '../../NavigationGuard/context';
import { DownOutlined, IdcardOutlined, LockOutlined, LogoutOutlined, UserOutlined } from '@ant-design/icons';
import { PasswordDialog } from '../../../features/account/PasswordDialog';
import { ProfileDialog } from '../../../features/account/ProfileDialog';
import adminStore from '../../../stores/AdminStore';
import styles from './index.module.scss';

/** 用户菜单拥有账号弹窗与离开确认；只在已通过身份守卫的页面布局内挂载。 */
export const UserMenu = observer(function UserMenu() {
  const store = adminStore;
  const [accountDialog, setAccountDialog] = useState<'profile' | 'password'>();
  const dialogTrigger = useRef<HTMLElement | null>(null);
  const currentUser = store.currentUser!;
  useLeaveConfirmation(accountDialog === 'password', () => setAccountDialog(undefined));
  // 弹窗卸载以清除秘密，由所属菜单恢复常驻按钮焦点；身份失效卸载整树，不再恢复焦点。
  useLayoutEffect(() => {
    if (accountDialog) return;
    const trigger = dialogTrigger.current;
    dialogTrigger.current = null;
    if (trigger?.isConnected) trigger.focus();
  }, [accountDialog]);
  return (
    <>
      <Dropdown
        trigger={['click']}
        disabled={store.busy}
        menu={{
          items: [
            { key: 'profile', icon: <IdcardOutlined />, label: '账号信息' },
            { key: 'password', icon: <LockOutlined />, label: '修改密码' },
            { type: 'divider' },
            { key: 'logout', icon: <LogoutOutlined />, label: '退出登录' },
          ],
          onClick: ({ key }) => {
            if (key === 'profile') setAccountDialog('profile');
            else if (key === 'password') setAccountDialog('password');
            else if (key === 'logout') void store.logout();
          },
        }}
      >
        <Button
          type="text"
          className={styles.account_button}
          disabled={store.busy}
          onClick={(event) => {
            // 菜单项关闭即消失，账号弹窗应回到常驻的账号按钮。
            dialogTrigger.current = event.currentTarget;
          }}
        >
          <Avatar size="small" icon={<UserOutlined />} />
          {currentUser.displayName}
          <DownOutlined />
        </Button>
      </Dropdown>
      {accountDialog === 'profile' && (
        // 账号信息以当前身份投影为准，改名称后由 saveUser 同步。
        <ProfileDialog user={currentUser} onClose={() => setAccountDialog(undefined)} />
      )}
      {accountDialog === 'password' && (
        <PasswordDialog onClose={() => setAccountDialog(undefined)} onSave={store.changePassword} />
      )}
    </>
  );
});
