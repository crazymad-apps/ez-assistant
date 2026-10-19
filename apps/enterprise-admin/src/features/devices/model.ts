import type { HostDeviceDetail, HostDeviceState, HostDeviceGrant } from '../../request/openapi';

export const statusLabels = {
  pending: '等待管理员确认',
  registered: '已登记',
  rejected: '申请已拒绝',
  withdrawn: '申请已撤回',
  revoked: '已注销',
};

export function editable(device: HostDeviceDetail): HostDeviceState {
  return {
    name: device.name,
    status: device.status,
    enabled: device.enabled,
    users: device.users.map((user) => ({ ...user })),
  };
}

export function sameState(a: HostDeviceState, b: HostDeviceState): boolean {
  const ordered = (users: HostDeviceGrant[]) => [...users].sort((a, b) => a.user_id - b.user_id);
  return (
    a.name === b.name &&
    a.status === b.status &&
    a.enabled === b.enabled &&
    JSON.stringify(ordered(a.users)) === JSON.stringify(ordered(b.users))
  );
}

export function reducedUsers(before: HostDeviceGrant[], after: HostDeviceGrant[]): number {
  return before.filter((old) => {
    const next = after.find((user) => user.user_id === old.user_id);
    return !next || (old.desktop_view && !next.desktop_view) || (old.desktop_control && !next.desktop_control);
  }).length;
}

/** 分区保存只替换本区字段；其它字段取提交前的中心快照，同区竞争仍由原 expected 拒绝。 */
export function sectionChange(
  base: HostDeviceState,
  draft: HostDeviceState,
  current: HostDeviceState,
  section: 'info' | 'users',
) {
  const comparable = section === 'info' ? { ...current, users: base.users } : { ...current, name: base.name };
  const expected = sameState(base, comparable) ? current : base;
  const desired = section === 'info' ? { ...expected, name: draft.name.trim() } : { ...expected, users: draft.users };
  return { expected, desired };
}
