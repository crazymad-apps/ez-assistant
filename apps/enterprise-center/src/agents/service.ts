import type { DataSource, EntityManager } from 'typeorm';
import type { AgentConfig, AgentDetail, AgentSummary } from '../contracts/agents.js';
import { IdentityError } from '../identity/errors.js';
import type { IdentityService } from '../identity/service.js';
import { PluginEntity, PluginPackageEntity } from '../plugins/entity.js';
import { agentConfig, agentEtag, agentInput, agentName, diagnoseAgent, requireAgentEtag } from './catalog.js';
import { CustomAgentEntity } from './entity.js';
import { AgentError } from './errors.js';

/** Agent 配置只保存名称引用；每次管理读取都按当前插件包事实重新诊断。 */
export class AgentsService {
  constructor(private readonly identity: IdentityService, private readonly source?: DataSource) {}

  private get manager(): EntityManager {
    if (!this.source?.isInitialized) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    return this.source.manager;
  }

  private async row(manager: EntityManager, name: string): Promise<CustomAgentEntity> {
    const found = await manager.getRepository(CustomAgentEntity).findOneBy({ name });
    if (!found) throw new AgentError(404, 'AGENT_NOT_FOUND');
    return found;
  }

  private async facts(manager: EntityManager) {
    const plugins = await manager.getRepository(PluginEntity).find();
    const packages = await manager.getRepository(PluginPackageEntity).find();
    return { plugins, packages };
  }

  /** 管理列表只返回摘要和即时诊断，不复制完整提示词到列表响应。 */
  list(token: string | undefined): Promise<AgentSummary[]> {
    return this.identity.write(async manager => {
      await this.identity.requireAdmin(manager, token);
      const agents = await manager.getRepository(CustomAgentEntity).find({ order: { name: 'ASC' } });
      const { plugins, packages } = await this.facts(manager);
      return agents.map(row => {
        const config = agentConfig(row);
        return { name: row.name, description: row.description,
          skill_count: row.skills.length, mcp_count: row.mcp.length,
          diagnostics: diagnoseAgent(config, plugins, packages) };
      });
    });
  }

  get(name: string, token: string | undefined): Promise<{ body: AgentDetail; etag: string }> {
    agentName(name);
    return this.identity.write(async manager => {
      await this.identity.requireAdmin(manager, token);
      const config = agentConfig(await this.row(manager, name));
      const { plugins, packages } = await this.facts(manager);
      return { body: { ...config, diagnostics: diagnoseAgent(config, plugins, packages) },
        etag: agentEtag(config) };
    });
  }

  /** 企业用户取得完整配置；不把诊断当作 Host 已安装/就绪结论。 */
  runtimeList(token: string | undefined): Promise<AgentConfig[]> {
    return this.identity.write(async manager => {
      await this.identity.requireUser(manager, token);
      return (await manager.getRepository(CustomAgentEntity).find({ order: { name: 'ASC' } }))
        .map(agentConfig);
    });
  }

  /** 仅新增的插件来源引用要求被至少一个当前包声明；旧失效引用可原样保留。 */
  private async validateNewReferences(manager: EntityManager, next: AgentConfig, before?: AgentConfig): Promise<void> {
    const packages = await manager.getRepository(PluginPackageEntity).find();
    for (const kind of ['skills', 'mcp'] as const) {
      for (const item of next[kind]) {
        if (!item.plugin || before?.[kind].some(previous =>
          previous.name === item.name && previous.plugin === item.plugin)) continue;
        if (!packages.some(row => row.plugin_name === item.plugin && row.manifest[kind].includes(item.name)))
          throw new AgentError(409, 'AGENT_DEPENDENCY_INVALID');
      }
    }
  }

  create(token: string | undefined, value: unknown, requestId: string): Promise<{
    body: AgentConfig; etag: string;
  }> {
    const input = agentInput(value);
    return this.identity.write(async manager => {
      const actor = await this.identity.requireAdmin(manager, token);
      const repo = manager.getRepository(CustomAgentEntity);
      if (await repo.findOneBy({ name: input.name })) throw new AgentError(409, 'AGENT_EXISTS');
      await this.validateNewReferences(manager, input);
      const now = new Date();
      await repo.save(repo.create({ ...input, created_at: now, updated_at: now }));
      await this.identity.audit(manager, 'agent_created', requestId, actor.id, undefined, null,
        { name: input.name, skill_count: input.skills.length, mcp_count: input.mcp.length });
      return { body: input, etag: agentEtag(input) };
    });
  }

  update(name: string, token: string | undefined, value: unknown, ifMatch: string | undefined,
    requestId: string): Promise<{ body: AgentConfig; etag: string }> {
    agentName(name);
    const input = agentInput(value);
    if (input.name !== name) throw new IdentityError(400, 'INVALID_REQUEST');
    return this.identity.write(async manager => {
      const actor = await this.identity.requireAdmin(manager, token);
      const row = await this.row(manager, name);
      const before = agentConfig(row);
      requireAgentEtag(ifMatch, agentEtag(before));
      if (agentEtag(input) !== agentEtag(before)) {
        await this.validateNewReferences(manager, input, before);
        row.description = input.description;
        row.prompt = input.prompt;
        row.skills = input.skills;
        row.mcp = input.mcp;
        row.updated_at = new Date();
        await manager.getRepository(CustomAgentEntity).save(row);
        await this.identity.audit(manager, 'agent_updated', requestId, actor.id, undefined, null,
          { name, skill_count: input.skills.length, mcp_count: input.mcp.length });
      }
      return { body: input, etag: agentEtag(input) };
    });
  }

  delete(name: string, token: string | undefined, ifMatch: string | undefined,
    requestId: string): Promise<void> {
    agentName(name);
    return this.identity.write(async manager => {
      const actor = await this.identity.requireAdmin(manager, token);
      const row = await this.row(manager, name);
      requireAgentEtag(ifMatch, agentEtag(row));
      await manager.getRepository(CustomAgentEntity).remove(row);
      // 删除配置不改写插件、其他 Agent 引用或 Host 已有会话。
      await this.identity.audit(manager, 'agent_deleted', requestId, actor.id, undefined, null, { name });
    });
  }
}
