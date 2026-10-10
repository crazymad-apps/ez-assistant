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
  feedback_created: '提交反馈',
  feedback_handled: '处理反馈',
  login: '登录',
  logout: '退出登录',
  self_password_changed: '修改本人密码',
  user_created: '创建用户',
  user_updated: '修改用户',
  user_deleted: '删除用户',
  user_password_reset: '管理员重置密码',
  users_read: '查询用户',
  audit_read: '查询审计',
  llm_recording_settings_changed: '修改调用记录策略',
  llm_snapshot_viewed: '查看调用快照',
  model_provider_created: '添加模型服务商',
  model_provider_updated: '修改模型服务商',
  model_provider_deleted: '删除模型服务商',
  model_catalog_refreshed: '刷新模型目录',
  model_configuration_saved: '保存模型固定配置',
  model_configuration_reset: '重置模型固定配置',
  model_default_changed: '更改企业默认模型',
  model_public_origin_changed: '更改客户端模型访问地址',
  model_templates_reloaded: '重新加载模型模板',
  agent_created: '创建智能体',
  agent_updated: '修改智能体',
  agent_deleted: '删除智能体',
  desktop_release_uploaded: '上传安装包',
  desktop_release_notes_changed: '修改更新说明',
  desktop_release_publication_changed: '更改安装包发布状态',
  plugin_updated: '修改插件说明',
  plugin_publication_changed: '更改插件上架状态',
  plugin_package_uploaded: '上传插件包',
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
  const details = record.details;
  if (details.feedback_id) changes.push(`反馈：${details.feedback_id}`);
  const feedbackStatuses: Record<string, string> = {
    pending: '待处理',
    processing: '处理中',
    resolved: '已解决',
    closed: '已关闭',
  };
  if (details.previous_status) changes.push(`原状态：${feedbackStatuses[details.previous_status]}`);
  if (details.status) changes.push(`当前状态：${feedbackStatuses[details.status]}`);
  if (details.provider_instance_id) changes.push(`服务商：${details.provider_instance_id}`);
  if (details.model_id) changes.push(`模型：${details.model_id}`);
  if (details.model_count !== undefined) changes.push(`在线模型数：${details.model_count}`);
  if (details.count !== undefined) changes.push(`模板条目数：${details.count}`);
  if (details.default_model !== undefined)
    changes.push(
      details.default_model
        ? `默认模型：${details.default_model.model_id}（${details.default_model.provider_instance_id}）`
        : '已清除默认模型',
    );
  if (details.public_origin !== undefined)
    changes.push(details.public_origin ? `客户端模型访问地址：${details.public_origin}` : '已恢复部署地址');
  // 只拼接中心审计白名单中的 C01 摘要，不展示完整提示词或任意 JSON 字段。
  if (details.name) changes.push(`${record.action.startsWith('agent_') ? '智能体' : '插件'}：${details.name}`);
  if (details.version) changes.push(`版本：${details.version}`);
  if (details.platforms?.length)
    changes.push(`平台：${details.platforms.map((item) => `${item.os}-${item.arch}`).join('、')}`);
  if (details.result)
    changes.push(
      `上传结果：${{ created: '新建版本', replaced: '替换当前包', unchanged: '内容未变化' }[details.result]}`,
    );
  if (details.publication)
    changes.push(`上架状态：${{ unpublished: '未上架', listed: '在架', deprecated: '已下架' }[details.publication]}`);
  if (details.sha256) changes.push(`SHA-256：${details.sha256}`);
  if (details.previous_sha256) changes.push(`原 SHA-256：${details.previous_sha256}`);
  if (details.skill_count !== undefined || details.mcp_count !== undefined)
    changes.push(`默认能力：Skill ${details.skill_count ?? 0} · MCP ${details.mcp_count ?? 0}`);
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
