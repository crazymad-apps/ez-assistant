export type Role = 'admin' | 'user';
export type PreviewState = 'normal' | 'empty' | 'failure';
export type Page = 'users' | 'audit';

export type DemoUser = Readonly<{
  id: string;
  username: string;
  displayName: string;
  role: Role;
  superAdmin: boolean;
  enabled: boolean;
  createdAt: string;
}>;

export type DemoAudit = Readonly<{
  id: string;
  time: string;
  actor: string;
  action: string;
  target: string;
  success: boolean;
  detail: string;
}>;

export type UserDraft = { username: string; displayName: string; role: Role; password?: string };
export const DEMO_PASSWORD = 'Demo@12345678';

export function createDemoUsers(): DemoUser[] {
  const names = [
    ['admin', '系统管理员'], ['lin.zhiyuan', '林致远'], ['chen.yu', '陈予'],
    ['zhou.yining', '周以宁'], ['xu.zhi', '许知'], ['jiang.wan', '江晚'],
    ['lu.jing', '陆景'], ['song.yan', '宋研'], ['wen.yu', '温予'],
    ['gu.xing', '顾行'], ['he.an', '何安'], ['shen.qing', '沈清'],
  ];
  return names.map(([username, displayName], index) => ({
    id: `demo-user-${index}`, username: username!, displayName: displayName!,
    role: index < 2 ? 'admin' : 'user', superAdmin: index === 0,
    enabled: index !== 7 && index !== 11,
    createdAt: `2026-09-${String(18 - index).padStart(2, '0')} 09:30`,
  }));
}

export function createDemoAudit(): DemoAudit[] {
  return [
    ['管理员登录', 'admin', true, '通过管理后台登录。'],
    ['创建用户', 'shen.qing', true, '角色：普通用户；状态：启用。'],
    ['停用用户', 'song.yan', true, '状态：启用 → 停用；该用户登录失效。'],
    ['管理员重置密码', 'chen.yu', true, '管理员设置新密码；目标用户登录失效。不记录密码内容。'],
    ['修改角色', 'lin.zhiyuan', true, '角色：普通用户 → 管理员。'],
    ['登录失败', '—', false, '认证失败；不区分账号不存在、密码错误或停用。'],
    ['修改本人密码', 'admin', true, '校验原密码后修改；本人登录失效。不记录密码内容。'],
  ].map(([action, target, success, detail], index) => ({
    id: `demo-audit-${index}`, time: `2026-09-18 09:${String(48 - index * 5).padStart(2, '0')}:12`,
    actor: success ? 'admin' : '未认证', action: String(action), target: String(target),
    success: Boolean(success), detail: String(detail),
  }));
}

export function userChangeError(users: readonly DemoUser[], target: DemoUser, next: DemoUser): string | undefined {
  if (target.superAdmin && (target.role !== next.role || target.enabled !== next.enabled)) {
    return '超级管理员的停用与降权规则尚待确认，原型暂不模拟这项操作。';
  }
  const enabledAdmins = users.filter(user => user.enabled && (user.role === 'admin' || user.superAdmin));
  if (enabledAdmins.length === 1 && enabledAdmins[0]?.id === target.id && (!next.enabled || next.role !== 'admin')) {
    return '至少需要保留一位启用的管理员。';
  }
  return undefined;
}
