import { createContext, useContext, useEffect } from 'react';

/** 仅登记当前挂载编辑器的离开回调，不保存表单或业务状态。 */
export class LeaveRequests {
  private readonly callbacks = new Set<() => void>();
  register(discard: () => void) {
    this.callbacks.add(discard);
    return () => {
      this.callbacks.delete(discard);
    };
  }
  get pending() {
    return this.callbacks.size > 0;
  }
  discard() {
    // 只关闭本次确认时的编辑器，回调触发卸载/重新登记不改变本次处理集合。
    const callbacks = [...this.callbacks];
    for (const discard of callbacks) discard();
  }
}

export const LeaveContext = createContext<LeaveRequests | null>(null);

/** 编辑状态仍归调用组件，只在需要确认离开期间登记，并随关闭/卸载释放。 */
export function useLeaveConfirmation(active: boolean, discard: () => void) {
  const requests = useContext(LeaveContext);
  if (!requests) throw new Error('缺少 NavigationGuard');
  useEffect(() => {
    if (active) return requests.register(discard);
  }, [requests, active, discard]);
}
