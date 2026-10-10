import { CenterError } from '../errors.js';

const messages = {
  DESKTOP_RELEASE_INVALID: '发布文件或请求格式不符合要求。',
  DESKTOP_RELEASE_SIGNATURE_INVALID: '发布清单签名校验失败。',
  DESKTOP_RELEASE_FILE_INVALID: '安装文件与发布清单不一致。',
  DESKTOP_RELEASE_FILE_UNAVAILABLE: '安装文件暂不可用。',
  DESKTOP_RELEASE_STORAGE_UNAVAILABLE: '安装包存储暂不可用。',
  DESKTOP_RELEASE_KEY_UNAVAILABLE: '尚未配置安装包签名公钥。',
  DESKTOP_RELEASE_NOT_FOUND: '安装版本不存在或尚未发布。',
  DESKTOP_RELEASE_IMMUTABLE: '已发布的安装文件不可覆盖。',
  DESKTOP_RELEASE_CHANGED: '版本内容已变化，请重新读取后核对。',
  DESKTOP_RELEASE_TIMEOUT: '安装包上传超时。',
  DESKTOP_RELEASE_CANCELLED: '安装包上传已取消。',
  FEEDBACK_NOT_FOUND: '反馈不存在。',
  FEEDBACK_SUBMISSION_CONFLICT: '该提交标识已用于不同内容，请核对原反馈。',
  FEEDBACK_CHANGED: '处理内容已变化，请重新读取后核对。',
  FEEDBACK_TOO_LARGE: '反馈内容或环境信息超过大小限制。',
  CALL_NOT_FOUND: '调用记录不存在。',
  PACKAGE_NOT_FOUND: '插件版本不存在。',
  SNAPSHOT_ROOT_UNAVAILABLE: '快照目录未配置或不可用。',
  MODEL_NOT_FOUND: '模型或服务商不存在，请刷新目录或保存固定配置。',
  MODEL_CONFIGURATION_CHANGED: '服务商连接已变化，请重新刷新。',
  MODEL_CONFIGURATION_INVALID: '模型参数不完整或能力组合无效。',
  MODEL_DISCOVERY_FAILED: '在线模型刷新失败，保留上次成功结果。',
  MODEL_TEMPLATES_INVALID: '模板读取或校验失败，保留当前有效模板。',
  INVALID_REQUEST: '请求字段或格式不符合要求。',
  AUTH_FAILED: '账号或密码错误，或账号不可用。',
  TOKEN_INVALID: '登录凭据无效，请重新登录。',
  ADMIN_REQUIRED: '此操作仅限管理员。',
  AUTH_STATE_CHANGED: '身份状态已变化，请重新操作。',
  RATE_LIMITED: '请求过于频繁，请稍后再试。',
  SERVICE_UNAVAILABLE: '中心服务暂时不可用。',
  USER_NOT_FOUND: '用户不存在。',
  USERNAME_EXISTS: '登录账号已存在。',
  SUPER_ADMIN_PROTECTED: '不能删除、停用超级管理员或将其降为普通用户。',
  LAST_ADMIN_REQUIRED: '至少保留一个启用的管理员。',
} as const;

export class IdentityError extends CenterError {
  constructor(
    readonly status: number,
    code: keyof typeof messages,
  ) {
    super(code, messages[code]);
  }
}
