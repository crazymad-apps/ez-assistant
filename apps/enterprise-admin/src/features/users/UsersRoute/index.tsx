import { useLayoutEffect, useRef, useState } from 'react';
import { observer } from 'mobx-react-lite';
import { App } from 'antd';
import { useLeaveConfirmation } from '../../../app/NavigationGuard/context';
import adminStore from '../../../stores/AdminStore';
import type { UserView } from '../../../model';
import { UserEditor } from '../UserEditor';
import { ResetPasswordDialog } from '../../passwords/ResetPasswordDialog';
import { UsersPage } from '../UsersPage';
import { useUsersData } from './useUsersData';

/** 用户管理路由页：列表数据、编辑与重置密码弹窗均为页面本地状态；写入成功后直接回调刷新列表。 */
export const UsersRoute = observer(function UsersRoute() {
  const { message } = App.useApp();
  const data = useUsersData();
  const [editor, setEditor] = useState<UserView | 'new'>();
  const [passwordTarget, setPasswordTarget] = useState<UserView>();
  const dialogTrigger = useRef<HTMLElement | null>(null);
  useLeaveConfirmation(Boolean(editor || passwordTarget), () => {
    setEditor(undefined);
    setPasswordTarget(undefined);
  });
  // 弹窗按目标卸载以清除表单秘密；卸载不走 Modal 关闭动画，由页面恢复触发按钮焦点。
  // 身份失效整树卸载本页面，不把焦点送回已失效后台；列表刷新移除的按钮也不再聚焦。
  useLayoutEffect(() => {
    if (editor || passwordTarget) return;
    const trigger = dialogTrigger.current;
    dialogTrigger.current = null;
    if (trigger?.isConnected) trigger.focus();
  }, [editor, passwordTarget]);

  function rememberDialogTrigger() {
    dialogTrigger.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  }

  return (
    <>
      <UsersPage
        {...data}
        busy={adminStore.busy}
        onCreate={() => {
          rememberDialogTrigger();
          setEditor('new');
        }}
        onEdit={(user) => {
          rememberDialogTrigger();
          setEditor(user);
        }}
        onResetPassword={(user) => {
          rememberDialogTrigger();
          setPasswordTarget(user);
        }}
        onToggle={async (user) => {
          const error = await adminStore.toggle(user);
          if (error) {
            void message.error(error);
            throw new Error('操作未成功确认');
          }
          // 停用自己已撤销身份，页面随即卸载，不再刷新。
          if (adminStore.user) {
            void message.success(user.enabled ? '用户已停用' : '用户已启用');
            data.onRecover();
          }
        }}
      />
      {editor && (
        <UserEditor
          key={editor === 'new' ? 'new' : editor.id}
          user={editor === 'new' ? undefined : editor}
          onBack={() => setEditor(undefined)}
          onSave={async (draft, user) => {
            const error = await adminStore.saveUser(draft, user);
            if (error) return error;
            setEditor(undefined);
            // 修改自己角色已撤销身份，页面随即卸载，不再刷新。
            if (adminStore.user) {
              void message.success(user ? '修改已保存' : '用户已创建，当前筛选可能不显示该用户');
              data.onRecover();
            }
          }}
        />
      )}
      {passwordTarget && (
        <ResetPasswordDialog
          user={passwordTarget}
          currentId={adminStore.currentUser!.id}
          onClose={() => setPasswordTarget(undefined)}
          onSave={async (user, password) => {
            const error = await adminStore.resetPassword(user, password);
            if (error) return error;
            setPasswordTarget(undefined);
            // 请求期间身份若已切换，不向新身份提交页面反馈。
            if (adminStore.user) {
              void message.success(`已重置 ${user.username} 的密码`);
              data.onRecover();
            }
          }}
        />
      )}
    </>
  );
});
