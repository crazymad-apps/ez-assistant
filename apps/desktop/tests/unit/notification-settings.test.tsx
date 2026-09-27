import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { RootStore } from '../../src/stores/RootStore';
import { RootStoreProvider } from '../../src/stores/RootStoreContext';
import { AppMessageProvider } from '../../src/components/AppMessage';
import { NotificationSettingsPage } from '../../src/features/settings/SettingsDialog/NotificationSettingsPage';
import { createNotificationPlatform } from '../../src/native-bridge/systemNotifications';

const stores: RootStore[] = [];
afterEach(() => {
  cleanup();
  for (const store of stores.splice(0)) store.dispose();
  localStorage.clear();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function showSettings() {
  const store = new RootStore();
  stores.push(store);
  render(
    <RootStoreProvider store={store}>
      <AppMessageProvider>
        <NotificationSettingsPage />
      </AppMessageProvider>
    </RootStoreProvider>,
  );
  return store;
}

it('不安全来源显示 HTTPS 指引，不能测试或请求权限', async () => {
  vi.stubGlobal('isSecureContext', false);
  const store = showSettings();
  await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('受信任的 HTTPS'));
  fireEvent.click(screen.getByRole('button', { name: '启用通知' }));
  await waitFor(() => expect(store.notifications.busy).toBe(false));
  expect(store.notifications.enabled).toBe(false);
  expect(screen.getByRole('button', { name: '发送测试通知' })).toBeDisabled();
});

it('拒绝权限不重复申请，撤销后刷新会禁用测试', async () => {
  let permission = 'denied';
  const requestPermission = vi.fn(async () => 'denied');
  vi.stubGlobal('isSecureContext', true);
  vi.stubGlobal('Notification', {
    get permission() {
      return permission;
    },
    requestPermission,
  });
  const platform = createNotificationPlatform();
  expect(await platform.permission(true)).toBe('denied');
  expect(requestPermission).not.toHaveBeenCalled();
  permission = 'granted';
  showSettings();
  fireEvent.click(screen.getByRole('button', { name: '启用通知' }));
  await waitFor(() => expect(screen.getByRole('button', { name: '发送测试通知' })).toBeEnabled());
  permission = 'denied';
  fireEvent.click(screen.getByRole('button', { name: '刷新权限' }));
  await waitFor(() => expect(screen.getByRole('button', { name: '发送测试通知' })).toBeDisabled());
  expect(requestPermission).not.toHaveBeenCalled();
});
