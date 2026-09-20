import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import { observer } from 'mobx-react-lite';
import { App } from 'antd';
import { useBlocker } from 'react-router';
import adminStore from '../../stores/AdminStore';
import { LeaveContext, LeaveRequests } from './context';

/** Router 只支持一个 blocker；同一身份下的编辑器共用此入口，页面布局保持纯组合。 */
export const NavigationGuard = observer(function NavigationGuard({ children }: { readonly children: ReactNode }) {
  const [requests] = useState(() => new LeaveRequests());
  const { message, modal } = App.useApp();
  const busy = adminStore.busy;
  const blocker = useBlocker(() => Boolean(adminStore.user && (adminStore.busy || requests.pending)));
  useEffect(() => {
    if (blocker.state !== 'blocked') return;
    if (busy) {
      blocker.reset();
      void message.info('操作正在提交，请稍候。');
      return;
    }
    const confirmation = modal.confirm({
      title: '离开当前编辑？',
      content: '尚未保存的修改将被放弃。',
      okText: '离开',
      cancelText: '继续编辑',
      onCancel: () => blocker.reset(),
      onOk: () => {
        // 确认框打开之后也可能开始写入，不能放行正在提交的操作。
        if (adminStore.busy) {
          blocker.reset();
          void message.info('操作正在提交，请稍候。');
          return;
        }
        requests.discard();
        blocker.proceed();
      },
    });
    return () => confirmation.destroy();
  }, [blocker, busy, message, modal, requests]);
  return <LeaveContext.Provider value={requests}>{children}</LeaveContext.Provider>;
});
