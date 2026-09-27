import { observer } from 'mobx-react-lite';
import { useEffect } from 'react';
import { Button } from '../../../components/Button';
import { useRootStore } from '../../../stores/RootStoreContext';
import { SettingsPageContainer } from './SettingsPageContainer';
import { SettingsMessages } from './SettingsMessages';
import type { NotificationPermission } from '../../notifications';

const permission_labels: Record<NotificationPermission, string> = {
  default: '尚未授权。点击启用通知后选择允许。',
  granted: '通知权限已开启。专注模式或系统通知样式可能影响横幅显示。',
  denied: '通知权限已关闭。请在系统通知设置或浏览器的网站设置中允许通知，再刷新权限。',
  insecure: '此地址不是安全来源。请使用受信任的 HTTPS 地址；内网 HTTP 不支持系统通知。',
  unsupported: '当前环境不支持系统通知。请使用 Chrome 或 Windows/macOS 桌面应用。',
  unavailable: '系统通知当前不可用。桌面端请使用应用包启动，并检查系统通知设置。',
};

export const NotificationSettingsPage = observer(function NotificationSettingsPage() {
  const notifications = useRootStore().notifications;
  useEffect(() => {
    void notifications.refreshPermission();
  }, [notifications]);
  return (
    <SettingsPageContainer
      title="系统通知"
      actions={
        <Button
          disabled={notifications.busy}
          onClick={() => {
            void notifications.refreshPermission();
          }}
        >
          刷新权限
        </Button>
      }
    >
      <SettingsMessages messages={notifications} />
      <p role="status">{permission_labels[notifications.permission]}</p>
      <p>本轮任务结束、需要审批或等待回复时，在当前设备提醒你。</p>
      <Button
        disabled={notifications.busy}
        variant={notifications.enabled ? 'outlined' : 'primary'}
        onClick={() => {
          if (notifications.enabled) notifications.setEnabled(false);
          else void notifications.enable();
        }}
      >
        {notifications.enabled ? '关闭通知' : '启用通知'}
      </Button>{' '}
      <Button
        disabled={!notifications.enabled || notifications.busy || notifications.permission !== 'granted'}
        onClick={() => {
          void notifications.test();
        }}
      >
        发送测试通知
      </Button>{' '}
      <Button
        disabled={!notifications.enabled || notifications.busy || notifications.permission !== 'granted'}
        onClick={() => {
          void notifications.test(true);
        }}
      >
        5 秒后发送
      </Button>
      <p>
        {notifications.platform.desktop
          ? '桌面应用运行且保持连接时接收通知，包括最小化和隐藏到托盘；退出应用后不接收新通知。'
          : '网页保持打开且连接有效时接收通知；页面冻结、关闭或浏览器退出后不保证接收。'}
      </p>
    </SettingsPageContainer>
  );
});
