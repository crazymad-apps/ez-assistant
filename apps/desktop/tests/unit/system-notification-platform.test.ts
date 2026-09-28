import { beforeEach, expect, it, vi } from 'vitest';
import { createNotificationPlatform } from '../../src/native-bridge/systemNotifications';

const bridge = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({
  isTauri: () => true,
  invoke: bridge.invoke,
  Channel: class {
    onmessage?: (event: string) => void;
  },
}));

beforeEach(() => {
  bridge.invoke.mockReset();
});

it('原生前台判断来自应用状态，不依赖页面输入控件焦点', async () => {
  vi.spyOn(document, 'hasFocus').mockReturnValue(false);
  bridge.invoke.mockResolvedValue(true);
  await expect(createNotificationPlatform().isFocused()).resolves.toBe(true);
  expect(bridge.invoke).toHaveBeenCalledWith('system_notification_focused');
  vi.restoreAllMocks();
});

it('查询后切回前台，原生跳过事件正常完成且不调用失败或撤回', async () => {
  bridge.invoke.mockImplementation(async (_command, args) => {
    args.events.onmessage('skipped');
  });
  const fail = vi.fn();
  const click = vi.fn();
  const close = await createNotificationPlatform().show('id', 'title', 'body', click, fail);
  close();
  expect(fail).not.toHaveBeenCalled();
  expect(click).not.toHaveBeenCalled();
  expect(bridge.invoke).toHaveBeenCalledOnce();
});

it('后台提交仍保留点击和撤回通知能力', async () => {
  let clicked!: () => void;
  bridge.invoke.mockImplementation(async (command, args) => {
    if (command === 'show_system_notification') {
      args.events.onmessage('submitted');
      clicked = () => args.events.onmessage('clicked');
    }
  });
  const click = vi.fn();
  const close = await createNotificationPlatform().show('id', 'title', 'body', click, vi.fn());
  clicked();
  expect(click).toHaveBeenCalledOnce();
  close();
  expect(bridge.invoke).toHaveBeenLastCalledWith('close_system_notification', { id: 'id' });
});
