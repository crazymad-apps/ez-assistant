import { CenterError } from '../errors.js';

const messages = {
  DEVICE_ACCESS_DENIED: '未获设备访问授权。',
  DEVICE_ACCESS_ENDED: '设备访问已结束，请重新发起。',
  DEVICE_NOT_FOUND: '设备不存在。',
  DEVICE_CONFLICT: '安装或登记标识已使用，请核实原登记。',
  DEVICE_CHANGED: '设备登记或授权已变化，请重新读取。',
  DEVICE_CENTER_MISMATCH: '中心身份不匹配。',
  DEVICE_CREDENTIAL_INVALID: '设备凭据无效。',
  SITE_CONTENT_INVALID: '内容格式、引用或资源不符合要求。',
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
  RECORDING_SETTINGS_OUTDATED: '记录策略格式已更新，请刷新管理页面后重试。',
  SNAPSHOT_READ_BUSY: '正文读取繁忙，请稍后重试。',
  SNAPSHOT_ROOT_UNAVAILABLE: '快照目录未配置或不可用。',
  MODEL_TEST_REQUIRES_CLIENT: 'OpenCode 推理测试请在客户端使用已保存配置执行；目录刷新只验证模型列表。',
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
