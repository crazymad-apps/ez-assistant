import { CenterError } from '../errors.js';

const messages = {
  INVALID_REQUEST: '请求字段或格式不符合要求。', AUTH_FAILED: '账号或密码错误，或账号不可用。',
  TOKEN_INVALID: '登录凭据无效，请重新登录。', ADMIN_REQUIRED: '此操作仅限管理员。',
  FORBIDDEN: '请求来源不被允许。', AUTH_STATE_CHANGED: '身份状态已变化，请重新操作。',
  RATE_LIMITED: '请求过于频繁，请稍后再试。', SERVICE_UNAVAILABLE: '中心服务暂时不可用。',
  USER_NOT_FOUND: '用户不存在。', USERNAME_EXISTS: '登录账号已存在。',
  SUPER_ADMIN_PROTECTED: '不能停用超级管理员或将其降为普通用户。', LAST_ADMIN_REQUIRED: '至少保留一个启用的管理员。',
} as const;

export class IdentityError extends CenterError {
  constructor(readonly status: number, code: keyof typeof messages) { super(code, messages[code]); }
}
