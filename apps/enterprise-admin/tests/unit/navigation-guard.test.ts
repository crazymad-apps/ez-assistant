import { expect, it, vi } from 'vitest';
import { LeaveRequests } from '../../src/app/NavigationGuard/context';

it('多个编辑来源共同阻止离开，关闭其中一个不会覆盖另一项，卸载后不再触发回调', () => {
  const requests = new LeaveRequests();
  const account = vi.fn(),
    user = vi.fn();
  const closeAccount = requests.register(account);
  const closeUser = requests.register(user);
  expect(requests.pending).toBe(true);
  closeUser();
  expect(requests.pending).toBe(true);
  requests.discard();
  expect(account).toHaveBeenCalledOnce();
  expect(user).not.toHaveBeenCalled();
  closeAccount();
  expect(requests.pending).toBe(false);
});
