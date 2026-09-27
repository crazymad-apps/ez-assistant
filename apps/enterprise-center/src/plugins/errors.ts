import { CenterError } from '../errors.js';

const messages = {
  PLUGIN_NOT_FOUND: '插件不存在。',
  PLUGIN_UNPUBLISHED: '插件尚未上架。',
  PLUGIN_DEPRECATED: '插件已下架。',
  PRECONDITION_REQUIRED: '请先读取最新插件信息后再提交。',
  CONFIG_CHANGED: '插件信息已变化，请重新读取后对照修改。',
  PACKAGE_CHANGED: '插件版本的当前包已变化，请重新选择。',
  PLATFORM_UNSUPPORTED: '没有支持目标平台的版本。',
  HOST_VERSION_UNSUPPORTED: '没有兼容目标 Host 版本的插件包。',
  CAPABILITY_MISSING: '没有包含所需能力的插件包。',
  PACKAGE_INVALID: '插件包结构或内容无效。',
  PACKAGE_TOO_LARGE: '插件包超过大小或条目限制。',
  PACKAGE_BUSY: '同时处理的插件包过多，请稍后重试。',
  PACKAGE_STORAGE_UNAVAILABLE: '插件包存储暂时不可用。',
  PACKAGE_UNAVAILABLE: '插件包文件不可用。',
  PACKAGE_RESULT_UNKNOWN: '发布结果无法确认，请重新读取插件版本。',
  PACKAGE_TIMEOUT: '插件包传输或校验超时。',
  PACKAGE_CANCELLED: '插件包上传已中断。',
} as const;

export class PluginError extends CenterError {
  constructor(readonly status: number, code: keyof typeof messages) {
    super(code, messages[code]);
  }
}

export const invalidPackage = () => new PluginError(400, 'PACKAGE_INVALID');
export const oversizedPackage = () => new PluginError(413, 'PACKAGE_TOO_LARGE');
