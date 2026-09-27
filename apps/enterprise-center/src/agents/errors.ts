import { CenterError } from '../errors.js';

const messages = {
  AGENT_NOT_FOUND: '智能体不存在。',
  AGENT_EXISTS: '智能体名称已存在。',
  AGENT_DEPENDENCY_INVALID: '新增的插件能力引用没有对应的当前版本声明。',
  PRECONDITION_REQUIRED: '请先读取最新智能体配置后再提交。',
  CONFIG_CHANGED: '智能体配置已变化，请重新读取后对照修改。',
} as const;

export class AgentError extends CenterError {
  constructor(readonly status: number, code: keyof typeof messages) {
    super(code, messages[code]);
  }
}
