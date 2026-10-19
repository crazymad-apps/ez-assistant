import { act, fireEvent, render, screen, waitFor, cleanup } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { runInAction } from 'mobx';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { LoginOptionsDialog } from '../../src/features/runtime-access/LoginOptionsDialog';
import { RuntimeConnectionForm } from '../../src/features/runtime-access/RuntimeConnectionForm';
import { ApplicationConnectionStore } from '../../src/features/runtime-access/ApplicationConnectionStore';
import { RootStore } from '../../src/stores/RootStore';
import type { LocalHostStatus } from '../../src/native-bridge/runtimeConnection';

vi.mock('@tauri-apps/api/core', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@tauri-apps/api/core')>()),
  isTauri: () => true,
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => undefined }));
vi.mock('../../src/native-bridge/desktopLifecycle', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../src/native-bridge/desktopLifecycle')>()),
  getDesktopPlatform: async () => 'macos',
}));

const baseline: LocalHostStatus = {
  instance_id: 'fixture',
  revision: 'revision-1',
  mode: 'enterprise',
  current_mode: 'enterprise',
  center_url: 'https://center.example.com',
  current_center_url: 'https://center.example.com',
  center_id: 'old-center',
  restart_required: false,
  error: null,
  password_configured: false,
  listener_state: 'listening',
  configuration: {
    port: 7240,
    scheme: 'http',
    remote_enabled: true,
    server_names: [],
    tls_certificate: null,
    tls_private_key: null,
  },
};
const entries: ApplicationConnectionStore[] = [];

function entry() {
  const connection = new ApplicationConnectionStore(new RootStore());
  entries.push(connection);
  runInAction(() => {
    connection.phase = 'entry';
    connection.mode = 'enterprise';
    connection.local_state = 'ready';
    connection.login_options = {
      phase: 'edit',
      baseline,
      draft: { mode: 'enterprise', center_url: baseline.center_url! },
      intended: null,
      expected_instance: 'fixture',
      recovery: null,
      message: null,
      address_error: null,
    };
  });
  return connection;
}

afterEach(() => {
  cleanup();
  entries.splice(0).forEach((connection) => connection.dispose());
  vi.restoreAllMocks();
});

describe('LoginOptionsDialog', () => {
  it('restores enterprise focus once, then preserves Tab navigation and password input', async () => {
    const user = userEvent.setup();
    const connection = entry();
    runInAction(() => {
      connection.login_options = null;
      connection.login_options_notice = '登录选项已生效';
    });
    render(<RuntimeConnectionForm connection={connection} />);
    const username = screen.getByLabelText('企业账号');
    const password = screen.getByLabelText('账号密码');
    await waitFor(() => expect(username).toHaveFocus());
    await user.type(username, 'fixture-user');
    await user.tab();
    expect(password).toHaveFocus();
    await user.keyboard('p');
    await act(() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())));
    expect(password).toHaveFocus();
    await user.keyboard('assword');
    expect(password).toHaveValue('password');
    expect(username).toHaveValue('fixture-user');
    act(() => { runInAction(() => { connection.pending = true; }); });
    await act(() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())));
    expect(password).toHaveFocus();
    act(() => { runInAction(() => { connection.pending = false; }); });
    await act(() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())));
    expect(password).toHaveFocus();
  });

  it('does not override focus chosen before the scheduled restoration runs', async () => {
    const connection = entry();
    runInAction(() => {
      connection.login_options = null;
      connection.login_options_notice = '登录选项已生效';
    });
    render(<RuntimeConnectionForm connection={connection} />);
    const password = screen.getByLabelText('账号密码');
    password.focus();
    await act(() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())));
    expect(password).toHaveFocus();
  });

  it('focuses personal entry after the final update check releases the pending state', async () => {
    const connection = entry();
    runInAction(() => {
      connection.mode = 'personal';
      connection.login_options = null;
      connection.login_options_notice = '登录选项已生效';
      connection.pending = true;
      connection.progress = '正在应用登录选项…';
    });
    render(<RuntimeConnectionForm connection={connection} />);
    const enter = screen.getByRole('button', { name: '正在应用登录选项…' });
    expect(enter).toBeDisabled();
    act(() => { runInAction(() => { connection.pending = false; }); });
    await waitFor(() => expect(enter).toHaveFocus());
  });

  it('keeps editing concise, retains an edited address across modes and requires a separate confirmation', async () => {
    const connection = entry();
    const apply = vi.spyOn(connection, 'applyLoginOptions').mockResolvedValue();
    render(<LoginOptionsDialog connection={connection} />);
    expect(screen.queryByText(/旧中心绑定/)).not.toBeInTheDocument();
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: '保存并重启' })).toBeEnabled();
    fireEvent.change(screen.getByLabelText('企业中心地址'), { target: { value: 'https://another.example.com/path' } });
    fireEvent.click(screen.getByRole('button', { name: '使用模式' }));
    fireEvent.click(screen.getByRole('option', { name: '个人模式' }));
    expect(screen.queryByLabelText('企业中心地址')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '使用模式' }));
    fireEvent.click(screen.getByRole('option', { name: '企业模式' }));
    expect(screen.getByLabelText('企业中心地址')).toHaveValue('https://another.example.com/path');
    fireEvent.click(screen.getByRole('button', { name: '保存并重启' }));
    expect(apply).not.toHaveBeenCalled();
    expect(screen.getByRole('dialog', { name: '确认应用登录选项' })).toBeVisible();
    await waitFor(() => expect(screen.getByRole('button', { name: '返回修改' })).toHaveFocus());
    expect(screen.getByText(/已有设备登记和用户数据保留/)).toBeVisible();
    expect(screen.getByText(/全部用户任务/)).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: '返回修改' }));
    expect(screen.getByLabelText('企业中心地址')).toHaveValue('https://another.example.com/path');
    expect(apply).not.toHaveBeenCalled();
  });

  it('focuses invalid addresses and ignores Enter during input composition', async () => {
    const connection = entry();
    render(<LoginOptionsDialog connection={connection} />);
    const input = screen.getByLabelText('企业中心地址');
    fireEvent.change(input, { target: { value: 'https://user:secret@example.com' } });
    fireEvent.click(screen.getByRole('button', { name: '保存并重启' }));
    expect(input).toHaveAttribute('aria-invalid', 'true');
    await waitFor(() => expect(input).toHaveFocus());
    fireEvent.change(input, { target: { value: 'https://another.example.com' } });
    fireEvent.compositionStart(input);
    expect(fireEvent.keyDown(input, { key: 'Enter', isComposing: true })).toBe(false);
    expect(connection.login_options?.phase).toBe('edit');
  });

  it('Escape closes the mode picker before the dialog, and cancelling never applies', async () => {
    const connection = entry();
    render(<LoginOptionsDialog connection={connection} />);
    fireEvent.click(screen.getByRole('button', { name: '使用模式' }));
    fireEvent.keyDown(screen.getByRole('option', { name: '企业模式' }), { key: 'Escape' });
    expect(connection.login_options).not.toBeNull();
    fireEvent.keyDown(screen.getByRole('button', { name: '使用模式' }), { key: 'Escape' });
    expect(connection.login_options).toBeNull();
  });

  it('prevents dismissing a write and clears the old password after a restart', () => {
    const connection = entry();
    render(
      <>
        <RuntimeConnectionForm connection={connection} />
        <LoginOptionsDialog connection={connection} />
      </>,
    );
    const password = screen.getByLabelText('账号密码');
    fireEvent.change(password, { target: { value: 'old-password' } });
    act(() => {
      runInAction(() => {
        connection.login_options = { ...connection.login_options!, phase: 'saving' };
      });
    });
    expect(screen.getByRole('button', { name: '关闭登录选项' })).toBeDisabled();
    fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    expect(connection.login_options).not.toBeNull();
    act(() => {
      runInAction(() => {
        connection.credentials_epoch += 1;
        connection.login_options = null;
        connection.login_options_notice = '登录选项已生效';
      });
    });
    expect(password).toHaveValue('');
    expect(screen.getByRole('status')).toHaveTextContent('登录选项已生效');
  });
});
