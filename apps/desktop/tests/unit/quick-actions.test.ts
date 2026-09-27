import { describe, expect, it, vi } from 'vitest';
import type { CustomAgentSnapshot, McpServerOptionSnapshot, SkillSummarySnapshot } from '@ez-assistant/protocol';
import { QUICK_ACTIONS, resolveQuickAction, type QuickActionConfig } from '../../src/features/composer/ComposerDock/quickActions';

describe('configured quick actions', () => {
  it('loads only the isolated test shortcuts', () => {
    expect(QUICK_ACTIONS.map((action) => action.label)).toEqual(['测试绘图入口', '测试咨询入口']);
  });

  it('resolves an agent, skill and MCP by current Host facts before applying a combined shortcut', async () => {
    const agent: CustomAgentSnapshot = {
      source: { type: 'center', center_id: 'center-1' },
      config: { name: '售后工程师助理', description: '', prompt: '帮助售后', skills: [], mcp: [] },
      revision: 'revision-1', created_at_ms: null, updated_at_ms: null,
    };
    const skill: SkillSummarySnapshot = {
      name: 'gorag-knowledge', plugin_name: 'gorag-knowledge', description: '企业知识库',
      source: 'plugin', model_invocable: true, user_invocable: true, enabled: true, health: 'ready',
    };
    const mcp: McpServerOptionSnapshot = {
      server_key: 'gorag-knowledge', plugin_name: 'gorag-knowledge', display_name: '企业知识库',
      description: '', visible_tool_count: 4,
    };
    const action: QuickActionConfig = {
      id: 'support', label: '售后咨询', icon: 'bot',
      agent: { name: agent.config.name, source: 'center' }, skill: skill.name, mcp: mcp.server_key,
    };
    const directory = {
      agents: vi.fn().mockResolvedValue([agent]),
      skills: vi.fn().mockResolvedValue([skill]),
      mcp: vi.fn().mockResolvedValue([mcp]),
    };
    await expect(resolveQuickAction(action, directory)).resolves.toEqual({
      agent: { name: agent.config.name, source: agent.source }, skill,
      mcp: { server_key: mcp.server_key, display_name: mcp.display_name },
    });
    expect(directory.agents).toHaveBeenCalledOnce();
    expect(directory.skills).toHaveBeenCalledOnce();
    expect(directory.mcp).toHaveBeenCalledOnce();
    await expect(resolveQuickAction({ ...action, mcp: 'missing' }, directory))
      .rejects.toThrow('MCP 服务「missing」当前不可用');
  });
});
