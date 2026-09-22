import { isAxiosError } from 'axios';
import * as sdk from './openapi/sdk.gen';
import type { CreateUserData, UpdateUserRequest, ListUsersData, ListManagementAuditData } from './openapi';

export type UserQuery = NonNullable<ListUsersData['query']>;
export type AuditQuery = NonNullable<ListManagementAuditData['query']>;

// 显式 Token 是每次异步操作的身份快照；公共请求配置和失效处理归 bootstrap。
// throwOnError 保留字面量以收窄生成 SDK 的响应类型，不另定义业务 DTO。
const options = (token: string, signal?: AbortSignal) => ({
  headers: { Authorization: `Bearer ${token}` },
  signal,
  throwOnError: true as const,
});

// 路径、方法、DTO 全部来自生成 SDK；这里只处理认证和调用方需要的响应体。
export const api = {
  login: async (username: string, password: string) =>
    (await sdk.login({ body: { username, password }, throwOnError: true })).data,

  me: async (token: string, signal?: AbortSignal) => (await sdk.getCurrentIdentity(options(token, signal))).data,

  logout: async (token: string) => {
    await sdk.logout({ ...options(token), body: {} });
  },

  password: async (token: string, old_password: string, new_password: string) => {
    await sdk.changeOwnPassword({ ...options(token), body: { old_password, new_password } });
  },

  users: async (token: string, query: UserQuery, signal: AbortSignal) =>
    (await sdk.listUsers({ ...options(token, signal), query })).data,

  audit: async (token: string, query: AuditQuery, signal: AbortSignal) =>
    (await sdk.listManagementAudit({ ...options(token, signal), query })).data,

  create: async (token: string, body: CreateUserData['body']) =>
    (await sdk.createUser({ ...options(token), body })).data,

  update: async (token: string, id: number, body: UpdateUserRequest) =>
    (await sdk.updateUser({ ...options(token), path: { id }, body })).data,

  reset: async (token: string, id: number, new_password: string) => {
    await sdk.resetUserPassword({ ...options(token), path: { id }, body: { new_password } });
  },
};

const errors: Record<string, string> = {
  CALL_NOT_FOUND: '调用记录不存在或已到期清理。',
  SNAPSHOT_ROOT_UNAVAILABLE: '快照目录未配置或不可用，请检查中心配置。',
  MODEL_NOT_FOUND: '模型或服务商不存在，请刷新目录或保存固定配置。',
  MODEL_CONFIGURATION_CHANGED: '服务商连接已变化，请重新刷新。',
  MODEL_CONFIGURATION_INVALID: '模型参数不完整或能力组合无效，请检查参数。',
  MODEL_DISCOVERY_FAILED: '在线模型刷新失败，保留上次成功结果。',
  MODEL_TEMPLATES_INVALID: '模板读取或校验失败，继续使用已有有效模板。',
  AUTH_FAILED: '账号或密码错误，或账号不可用。',
  TOKEN_INVALID: '登录已失效，请重新登录。',
  ADMIN_REQUIRED: '此账号无管理后台权限，请通过 Runtime 登录。',
  USERNAME_EXISTS: '登录账号已存在，请更换账号。',
  SUPER_ADMIN_PROTECTED: '不能停用超级管理员或将其降为普通用户。',
  LAST_ADMIN_REQUIRED: '至少需要保留一位启用的管理员。',
  USER_NOT_FOUND: '该用户不存在，请刷新列表。',
  INVALID_REQUEST: '输入格式不符合要求，请检查后再提交。',
  RATE_LIMITED: '操作过于频繁，请稍后重试。',
  AUTH_STATE_CHANGED: '账号信息已变更，请重新核对。',
  FORBIDDEN: '请求来源不被允许，请检查访问地址。',
};

export function failure(error: unknown, write = false) {
  const value: unknown = isAxiosError(error) ? error.response?.data : undefined;
  const envelope: unknown = value && typeof value === 'object' ? Reflect.get(value, 'error') : undefined;
  const code: unknown = envelope && typeof envelope === 'object' ? Reflect.get(envelope, 'code') : undefined;
  const requestId: unknown = envelope && typeof envelope === 'object' ? Reflect.get(envelope, 'request_id') : undefined;
  const known = typeof code === 'string' ? errors[code] : undefined;
  const text =
    known ?? (write ? '操作结果未确认，请核对列表或审计后再决定是否重试。' : '暂时无法连接企业中心，请重试。');
  return {
    code,
    text: text + (typeof requestId === 'string' && /^[a-f0-9-]{36}$/i.test(requestId) ? `（请求 ${requestId}）` : ''),
  };
}
