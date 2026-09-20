import type { IdentityUser, UserRecord, AuditRecord } from './request/openapi';

export type Role = 'admin' | 'user';
export type LoadState = 'normal' | 'loading' | 'failure';
export type UserView = Readonly<{
  id: number;
  username: string;
  displayName: string;
  role: Role;
  superAdmin: boolean;
  enabled: boolean;
  createdAt: string;
}>;
export type UserDraft = { username: string; displayName: string; role: Role; password?: string };
export type AuditView = Readonly<{
  id: number;
  time: string;
  actor: string;
  target: string;
  action: string;
  success: boolean;
  detail: string;
  requestId: string;
}>;
export const actionLabels: Record<string, string> = {
  center_initialized: '中心初始化',
  login: '登录',
  logout: '退出登录',
  self_password_changed: '修改本人密码',
  user_created: '创建用户',
  user_updated: '修改用户',
  user_password_reset: '管理员重置密码',
  users_read: '查询用户',
  audit_read: '查询审计',
};
export function userView(user: IdentityUser | UserRecord): UserView {
  return {
    id: user.id,
    username: user.username,
    displayName: user.display_name,
    role: user.role,
    superAdmin: user.is_super_admin,
    enabled: user.enabled,
    createdAt: 'created_at' in user ? new Date(user.created_at).toLocaleString() : '—',
  };
}
export function auditView(record: AuditRecord): AuditView {
  // 不猜测失败登录账号，也不把任意 JSON 字段直接展示；只渲染正式契约白名单。
  const changes: string[] = [];
  for (const key of ['before', 'after'] as const) {
    const value = record.details[key];
    if (!value) continue;
    const parts = [
      value.display_name,
      value.role === undefined ? undefined : value.role === 'admin' ? '管理员' : '普通用户',
      value.enabled === undefined ? undefined : value.enabled ? '启用' : '停用',
    ].filter((item) => item !== undefined);
    changes.push(`${key === 'before' ? '变更前' : '变更后'}：${parts.join(' / ')}`);
  }
  return {
    id: record.id,
    time: new Date(record.occurred_at).toLocaleString(),
    actor: record.actor_username ?? (record.actor_user_id === null ? '未认证' : `用户 #${record.actor_user_id}`),
    target: record.target_username ?? (record.target_user_id === null ? '—' : `用户 #${record.target_user_id}`),
    action: actionLabels[record.action] ?? record.action,
    success: record.success,
    detail:
      changes.join('；') || (record.reason_code ? `原因：${record.reason_code}` : '操作已记录，不保存密码或凭据内容。'),
    requestId: record.request_id,
  };
}
