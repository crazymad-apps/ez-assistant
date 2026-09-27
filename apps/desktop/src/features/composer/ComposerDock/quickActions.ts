import type {
  CustomAgentSelection,
  CustomAgentSnapshot,
  McpSelectionTagSnapshot,
  McpServerOptionSnapshot,
  SkillSummarySnapshot,
} from '@ez-assistant/protocol';
import type { IconName } from '../../../components/Icon';

export type QuickActionConfig = Readonly<{
  id: string;
  label: string;
  icon: IconName;
  template?: string;
  agent?: Readonly<{ name: string; source: 'personal' | 'center' }>;
  skill?: string;
  mcp?: string;
  picker?: 'agent' | 'skill' | 'mcp';
}>;

export type ResolvedQuickAction = Readonly<{
  agent?: CustomAgentSelection;
  skill?: SkillSummarySnapshot;
  mcp?: McpSelectionTagSnapshot;
}>;

type QuickActionDirectory = Readonly<{
  agents: () => Promise<readonly CustomAgentSnapshot[]>;
  skills: () => Promise<readonly SkillSummarySnapshot[]>;
  mcp: () => Promise<readonly McpServerOptionSnapshot[]>;
}>;

/** 本机模板在开发预览或企业版构建时注入，不进入通用 Composer 分支。 */
if (!Array.isArray(__QUICK_ACTIONS_CONFIG__)) throw new Error('快捷入口配置无效');
export const QUICK_ACTIONS: readonly QuickActionConfig[] = __QUICK_ACTIONS_CONFIG__.map((value: unknown) => {
  const action = value as QuickActionConfig;
  if (!action.id || !action.label || !action.icon ||
    !(action.template || action.agent || action.skill || action.mcp || action.picker)) {
    throw new Error('快捷入口配置无效');
  }
  return action;
});

/** 固定引用只从当前 Host 目录解析；任何一项缺失都不产生部分草稿变更。 */
export async function resolveQuickAction(
  action: QuickActionConfig,
  directory: QuickActionDirectory,
): Promise<ResolvedQuickAction> {
  const [agents, skills, mcp] = await Promise.all([
    action.agent ? directory.agents() : Promise.resolve([]),
    action.skill ? directory.skills() : Promise.resolve([]),
    action.mcp ? directory.mcp() : Promise.resolve([]),
  ]);
  const agent = action.agent && agents.find((item) =>
    item.config.name === action.agent?.name && item.source.type === action.agent.source);
  if (action.agent && !agent) throw new Error(`智能体「${action.agent.name}」当前不可用`);
  const skill = action.skill && skills.find((item) => item.name === action.skill
    && item.enabled && item.user_invocable && item.health === 'ready');
  if (action.skill && !skill) throw new Error(`技能「${action.skill}」当前不可用`);
  const server = action.mcp && mcp.find((item) => item.server_key === action.mcp);
  if (action.mcp && !server) throw new Error(`MCP 服务「${action.mcp}」当前不可用`);
  return {
    ...(agent ? { agent: { name: agent.config.name, source: agent.source } } : {}),
    ...(skill ? { skill } : {}),
    ...(server ? { mcp: { server_key: server.server_key, display_name: server.display_name } } : {}),
  };
}
