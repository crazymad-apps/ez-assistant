import { useEffect } from 'react';
import { observer } from 'mobx-react-lite';
import { Button, Result } from 'antd';
import { Outlet } from 'react-router';
import adminStore from '../../stores/AdminStore';

/** 身份变化通过卸载整个工作区清除表单和页面投影，不保留前一账号的弹窗草稿。 */
export const AdminApp = observer(function AdminApp() {
  const store = adminStore;
  useEffect(() => {
    const refresh = () => {
      if (document.visibilityState === 'visible') void store.verifyIdentity();
    };
    document.addEventListener('visibilitychange', refresh);
    return () => {
      document.removeEventListener('visibilitychange', refresh);
      // Store 属于 bootstrap；StrictMode 的 effect 清理不应销毁已恢复的登录态。
    };
  }, [store]);
  if (store.restoreState !== 'none')
    return (
      <Result
        status="info"
        title={store.restoreState === 'pending' ? '正在恢复登录' : '暂时无法恢复登录'}
        subTitle="正在核对企业中心身份，确认前不显示管理数据。"
        extra={
          <Button loading={store.restoreState === 'pending'} onClick={() => void store.verifyIdentity()}>
            重试
          </Button>
        }
      />
    );
  if (store.logoutState !== 'none')
    return (
      <Result
        status="info"
        title={store.logoutState === 'pending' ? '正在退出登录' : '退出尚未确认'}
        subTitle="管理数据已隐藏。只有服务器确认后才视为退出成功。"
        extra={
          <Button loading={store.logoutState === 'pending'} onClick={store.logout}>
            重试退出
          </Button>
        }
      />
    );
  return <Outlet />;
});
