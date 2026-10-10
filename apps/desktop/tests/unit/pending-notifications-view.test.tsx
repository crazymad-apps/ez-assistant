import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ApprovalSnapshot } from '@ez-assistant/protocol';
import { PendingNotificationStore } from '../../src/features/notifications/PendingNotificationStore';
import { PendingNotifications } from '../../src/features/notifications/PendingNotifications';
import { RootStoreProvider } from '../../src/stores/RootStoreContext';
import type { RootStore } from '../../src/stores/RootStore';

const stores: PendingNotificationStore[] = [];
afterEach(() => { cleanup(); stores.splice(0).forEach((store) => store.dispose()); });
function fixture() {
  const store = new PendingNotificationStore({ client: () => null, selectedSession: () => 'current',
    title: () => '项目中的其他会话', feedback: { requests: new Map(), errors: new Map(), refresh: async () => {} } });
  store.setConnection('scope', true); stores.push(store);
  const approval = { approval_id: 'approval', session_id: 'other', run_id: 'run', subject: { type: 'shell', tool_name: 'shell' } } as ApprovalSnapshot;
  store.ingest({ sequence: 1, emitted_at_ms: 1, event: { type: 'approval_requested', approval } });
  const activate = vi.fn(async () => true);
  const root = { pending_notifications: store, notifications: { activate, enabled: false }, showInteractionError: vi.fn() } as unknown as RootStore;
  render(<RootStoreProvider store={root}><PendingNotifications /></RootStoreProvider>);
  return { store, root, activate };
}

describe('壳层待处理入口', () => {
  it('系统通知关闭时仍显示短提示与分组菜单；关闭提示不移除事项', async () => {
    const { activate } = fixture();
    expect(screen.getByRole('status')).toHaveTextContent('需要审批：shell');
    fireEvent.click(screen.getByRole('button', { name: '关闭提醒' }));
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
    const trigger = screen.getByRole('button', { name: '待处理 1' });
    fireEvent.keyDown(trigger, { key: 'ArrowDown' });
    const menu = await screen.findByRole('menu', { name: '待处理事项' });
    expect(within(menu).getByRole('group', { name: '项目中的其他会话' })).toBeInTheDocument();
    const item = within(menu).getByRole('menuitem', { name: '需要审批：shell' });
    await waitFor(() => expect(item).toHaveFocus());
    fireEvent.keyDown(item, { key: 'Escape' });
    expect(trigger).toHaveFocus();
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole('menuitem', { name: '需要审批：shell' }));
    expect(activate).toHaveBeenCalledWith(expect.objectContaining({ kind: 'approval', approval_id: 'approval', scope: 'scope' }));
  });

  it('断线时显示尚未同步并禁用事项，删除事件收回提示和入口', async () => {
    const { store } = fixture();
    act(() => store.setConnection('scope', false));
    fireEvent.click(screen.getByRole('button', { name: '待处理 1 · 未同步' }));
    expect(screen.getByText('待处理事项尚未同步')).toBeVisible();
    expect(screen.getByRole('menuitem', { name: '需要审批：shell' })).toBeDisabled();
    act(() => store.ingest({ sequence: 2, emitted_at_ms: 2, event: { type: 'session_deleted', session_id: 'other' } }));
    expect(screen.queryByRole('button', { name: '待处理 1' })).not.toBeInTheDocument();
  });
});
