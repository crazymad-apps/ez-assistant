import { ControlError } from '../llm/errors.js';
import type { Protocol } from '../contracts/models.js';
import { randomUUID } from 'node:crypto';
import type { DataSource, EntityManager } from 'typeorm';
import type {
  ModelAvailability,
  ModelConfiguration,
  ModelPolicy,
  ModelSelection,
  ModelSettings,
  ProviderConnection,
  ProviderSummary,
  RuntimeModelConfiguration,
  RuntimeModelProvider,
  SaveProvider,
} from '../contracts/models.js';
import { IdentityService } from '../identity/service.js';
import { IdentityError } from '../identity/errors.js';
import { fields, invalid } from '../validation.js';
import { ModelTemplates } from './templates.js';
import { organizations } from '../organizations/repository.js';
import { models } from './repository.js';
import type { FixedConfigEntity, ProviderEntity } from './entity.js';
import {
  emptyParameters,
  enumeration,
  executable,
  mergeParameters,
  parseConnection,
  parseParameters,
  parsePublicOrigin,
  resolveProtocol,
  text,
  uuid,
  validParameters,
} from './parameters.js';
import { discover } from './discovery.js';
import { configurationIdentity, historyBinding, historyConnectionIdentity, serverHistorySupport } from './history.js';
import type { RequestModel } from '../llm/body.js';

const summary = (row: ProviderEntity): ProviderSummary => ({
  provider_instance_id: row.provider_instance_id,
  connection: row.connection,
  has_api_key: Boolean(row.api_key),
});

const selectionInput = (value: unknown): ModelSelection => {
  const input = fields(value, ['provider_instance_id', 'model_id']);
  return { provider_instance_id: uuid(input.provider_instance_id), model_id: text(input.model_id, 1024) };
};

const selectionKey = (selection: ModelSelection) => JSON.stringify([selection.provider_instance_id, selection.model_id]);
const sameSelection = (left: ModelSelection | null, right: ModelSelection | null) =>
  left === null || right === null ? left === right : selectionKey(left) === selectionKey(right);

function effectiveModels(policy: ModelPolicy): ModelSelection[] {
  if (policy.allow_member_selection) return policy.allowed_models;
  return policy.default_model ? [policy.default_model] : [];
}

function policyInput(value: unknown): ModelPolicy {
  const input = fields(value, ['default_model', 'allow_member_selection', 'allowed_models', 'revision']);
  if (typeof input.allow_member_selection !== 'boolean' || !Array.isArray(input.allowed_models) ||
    input.allowed_models.length > 1000) invalid();
  const allowed_models = input.allowed_models.map(selectionInput);
  const keys = new Set(allowed_models.map(selectionKey));
  const default_model = input.default_model === null ? null : selectionInput(input.default_model);
  if (keys.size !== allowed_models.length ||
    (default_model === null ? keys.size !== 0 : !keys.has(selectionKey(default_model)))) invalid();
  return {
    allow_member_selection: input.allow_member_selection,
    allowed_models,
    default_model,
    revision: input.revision === null ? null : uuid(input.revision),
  };
}

function parseSave(value: unknown): SaveProvider {
  const input = fields(value, ['connection', 'credential']),
    c = fields(input.credential, ['mode'], ['value']);
  const mode = enumeration(c.mode, ['unchanged', 'replace', 'clear']);
  if (mode !== 'replace' && Object.hasOwn(c, 'value')) invalid();
  return {
    connection: parseConnection(input.connection),
    credential: { mode, ...(mode === 'replace' ? { value: text(c.value, 16384, true) } : {}) },
  };
}

const discoveryIdentity = (connection: ProviderConnection, key: string) =>
  JSON.stringify([
    connection.provider_type,
    connection.endpoint,
    connection.models_path,
    connection.discovery_format,
    key,
  ]);

/** PostgreSQL 持有模型事实；在同一身份串行段中求值/写入/发布模板，不缓存第二份有效配置。 */
export class ModelsService {
  private readonly shutdown = new AbortController();
  private readonly refreshes = new Map<string, Promise<unknown>>();
  private readonly invalidated = new Set<(provider: string) => void>();

  constructor(
    private readonly identity: IdentityService,
    private readonly source: DataSource | undefined,
    readonly templates: ModelTemplates,
    private readonly origin?: string,
  ) {}

  private get manager() {
    if (!this.source) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    return this.source.manager;
  }

  /** 配置持久提交后同步撤销传输资源；监听方不保存另一份模型配置。 */
  onHistoryInvalidated(notify: (provider: string) => void): () => void {
    this.invalidated.add(notify);
    return () => {
      this.invalidated.delete(notify);
    };
  }

  private invalidate(providers: Set<string>): void {
    for (const provider of providers) for (const notify of this.invalidated) notify(provider);
  }

  async onModuleDestroy(): Promise<void> {
    this.shutdown.abort();
    await Promise.allSettled(this.refreshes.values());
  }

  private async provider(manager: EntityManager, organizationId: string, id: string) {
    const row = await models.provider(manager, organizationId, uuid(id));
    if (!row) throw new IdentityError(404, 'MODEL_NOT_FOUND');
    return row;
  }

  private read<T>(token: string | undefined, work: (manager: EntityManager, organizationId: string) => Promise<T>): Promise<T> {
    return this.identity.write(async (manager) => {
      const { organizationId } = await this.identity.requireOrganizationAdmin(manager, token);
      return work(manager, organizationId);
    });
  }

  private write<T>(
    token: string | undefined,
    requestId: string,
    action: string,
    work: (manager: EntityManager, organizationId: string, invalidate: (provider: string) => void) => Promise<T>,
    details: object = {},
  ): Promise<T> {
    const invalidated = new Set<string>();
    return this.identity.write(
      async (manager) => {
        const { user: actor, organizationId } = await this.identity.requireOrganizationAdmin(manager, token);
        const result = await work(manager, organizationId, (provider) => {
          invalidated.add(provider);
        });
        await this.identity.audit(manager, action, requestId, actor.id, undefined, null, details, organizationId);
        return result;
      },
      () => this.invalidate(invalidated),
    );
  }

  async list(token?: string) {
    return this.read(token, async (manager, organizationId) => {
      // 列表明确只选择凭据是否存在，不能将秘密 Entity 序列化出站。
      const rows = await manager
        .getRepository('providers')
        .createQueryBuilder('p')
        .select(['p.provider_instance_id', 'p.connection'])
        .addSelect("p.api_key <> ''", 'has_api_key')
        .where('p.organization_id = :organizationId', { organizationId })
        .orderBy('p.provider_instance_id')
        .getRawMany<{ p_provider_instance_id: string; p_connection: ProviderConnection; has_api_key: boolean }>();
      return rows.map((row) => ({
        provider_instance_id: row.p_provider_instance_id,
        connection: row.p_connection,
        has_api_key: row.has_api_key,
      }));
    });
  }

  get(token: string | undefined, id: string) {
    return this.read(token, async (manager, organizationId) => summary(await this.provider(manager, organizationId, id)));
  }

  save(token: string | undefined, id: string | undefined, value: unknown, requestId: string) {
    const input = parseSave(value),
      providerId = id === undefined ? randomUUID() : uuid(id);
    return this.write(
      token,
      requestId,
      id === undefined ? 'model_provider_created' : 'model_provider_updated',
      async (manager, organizationId, invalidate) => {
        const previous = id === undefined ? null : await this.provider(manager, organizationId, providerId);
        // 旧 Admin 缺少传输字段时不得把已启用的 WebSocket 静默改回 HTTP。
        if (
          previous?.connection.responses_transport === 'websocket' &&
          !Object.hasOwn((value as { connection: object }).connection, 'responses_transport')
        )
          invalid();
        let key = previous?.api_key ?? '';
        if (input.credential.mode === 'replace') key = input.credential.value!;
        if (input.credential.mode === 'clear') key = '';
        if (input.connection.provider_type === 'dashscope_api' && key.startsWith('sk-sp-')) invalid();
        if (!previous) await models.insert(manager, organizationId, providerId, input.connection, key);
        else {
          const changed =
            discoveryIdentity(previous.connection, previous.api_key) !== discoveryIdentity(input.connection, key);
          let server_history_key = previous.server_history_key;
          if (!server_history_key || historyConnectionIdentity(previous.connection, previous.api_key) !== historyConnectionIdentity(input.connection, key))
            server_history_key = randomUUID();
          if (server_history_key !== previous.server_history_key) invalidate(providerId);
          await models.update(manager, {
            ...previous,
            connection: input.connection,
            api_key: key,
            server_history_key,
            catalog_connection_changed:
              previous.catalog_connection_changed || (previous.model_catalog !== null && changed),
          });
        }
        return summary(await this.provider(manager, organizationId, providerId));
      },
      { provider_instance_id: providerId },
    );
  }

  private async usageIn(manager: EntityManager, organizationId: string, id: string) {
    await this.provider(manager, organizationId, id);
    const policy = await models.policy(manager, organizationId);
    return {
      fixed_config_count: await models.fixedCount(manager, id),
      default_model: policy.default_model?.provider_instance_id === id,
      allowed_model_ids: policy.allowed_models.filter((item) => item.provider_instance_id === id).map((item) => item.model_id),
    };
  }

  usage(token: string | undefined, id: string) {
    return this.read(token, (manager, organizationId) => this.usageIn(manager, organizationId, id));
  }

  remove(token: string | undefined, id: string, requestId: string) {
    return this.write(
      token,
      requestId,
      'model_provider_deleted',
      async (manager, organizationId, invalidate) => {
        const usage = await this.usageIn(manager, organizationId, id);
        await models.remove(manager, id);
        invalidate(id);
        return usage;
      },
      { provider_instance_id: uuid(id) },
    );
  }

  catalog(token: string | undefined, id: string) {
    return this.read(token, async (manager, organizationId) => this.catalogView(await this.provider(manager, organizationId, id)));
  }

  private catalogView(row: ProviderEntity) {
    return {
      provider_instance_id: row.provider_instance_id,
      models: (row.model_catalog ?? []).map((model) => {
        const merged = mergeParameters(
          model.metadata,
          this.templates.lookup(row.connection, model.model_id).parameters,
        );
        return {
          ...model,
          configuration: {
            uses_template: Object.values(merged.sources).includes('template'),
            requires_configuration: !executable(row.connection, merged.parameters),
          },
        };
      }),
      refreshed_at_ms: row.catalog_refreshed_at?.getTime() ?? null,
      connection_changed: row.catalog_connection_changed,
    };
  }

  async refresh(token: string | undefined, id: string, requestId: string) {
    uuid(id);
    const { organizationId } = await this.identity.requireOrganizationAdmin(this.manager, token);
    const refreshKey = `${organizationId}:${id}`;
    const existing = this.refreshes.get(refreshKey);
    if (existing) {
      await existing;
      return this.catalog(token, id);
    }
    if (this.shutdown.signal.aborted) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    const pending = (async () => {
      const captured = await this.provider(this.manager, organizationId, id);
      const catalog = await discover(captured.connection, captured.api_key, this.shutdown.signal);
      return this.write(
        token,
        requestId,
        'model_catalog_refreshed',
        async (manager, organizationId, invalidate) => {
          const current = await this.provider(manager, organizationId, id);
          if (this.shutdown.signal.aborted) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
          if (
            discoveryIdentity(current.connection, current.api_key) !==
            discoveryIdentity(captured.connection, captured.api_key)
          )
            throw new IdentityError(409, 'MODEL_CONFIGURATION_CHANGED');
          const updated = { ...current, model_catalog: catalog };
          await models.catalog(manager, id, catalog);
          const policy = await models.policy(manager, organizationId);
          for (const selection of policy.allowed_models.filter((item) => item.provider_instance_id === id)) {
            const before = await this.evaluate(manager, current, selection);
            const after = await this.evaluate(manager, updated, selection);
            if (this.executionChanged(before, after)) {
              await models.renewHistory(manager, current);
              invalidate(id);
              break;
            }
          }
          return this.catalogView(await this.provider(manager, organizationId, id));
        },
        { provider_instance_id: id, model_count: catalog.length },
      );
    })();
    this.refreshes.set(refreshKey, pending);
    try {
      return await pending;
    } finally {
      if (this.refreshes.get(refreshKey) === pending) this.refreshes.delete(refreshKey);
    }
  }

  private fixedView(row: FixedConfigEntity, connection: ProviderConnection): ModelConfiguration {
    return {
      provider_instance_id: row.provider_instance_id,
      model_id: row.model_id,
      origin: row.origin,
      parameters: row.parameters,
      source: 'fixed',
      field_sources: {},
      updated_at_ms: row.updated_at.getTime(),
      template_document: null,
      template_checked_on: null,
      requires_configuration: !executable(connection, row.parameters),
    };
  }

  private async configurationIn(
    manager: EntityManager,
    organizationId: string,
    selection: ModelSelection,
    origin: 'online' | 'manual',
    templates?: Awaited<ReturnType<ModelTemplates['read']>>,
  ): Promise<ModelConfiguration> {
    const provider = await this.provider(manager, organizationId, selection.provider_instance_id);
    const configuration = await this.resolveConfiguration(manager, provider, selection, origin, templates);
    if (!configuration) throw new IdentityError(404, 'MODEL_NOT_FOUND');
    return configuration;
  }

  private async resolveConfiguration(
    manager: EntityManager,
    provider: ProviderEntity,
    selection: ModelSelection,
    origin: 'online' | 'manual',
    templates?: Awaited<ReturnType<ModelTemplates['read']>>,
  ): Promise<ModelConfiguration | null> {
    const fixed = await models.fixed(manager, selection);
    if (fixed) return this.fixedView(fixed, provider.connection);
    const online = provider.model_catalog?.find((model) => model.model_id === selection.model_id);
    if (!online && origin !== 'manual') return null;
    const template = this.templates.lookup(provider.connection, selection.model_id, templates);
    const merged = mergeParameters(online?.metadata ?? emptyParameters(), template.parameters);
    return {
      ...selection,
      origin,
      parameters: merged.parameters,
      source: 'online',
      field_sources: merged.sources,
      updated_at_ms: null,
      template_document: template.document,
      template_checked_on: template.checked_on,
      requires_configuration: !executable(provider.connection, merged.parameters),
    };
  }

  query(token: string | undefined, value: unknown) {
    const input = fields(value, ['provider_instance_id', 'model_id'], ['origin']);
    const selection = selectionInput({ provider_instance_id: input.provider_instance_id, model_id: input.model_id });
    const origin = enumeration(input.origin ?? 'online', ['online', 'manual']);
    return this.read(token, (manager, organizationId) => this.configurationIn(manager, organizationId, selection, origin));
  }

  fixedPage(token: string | undefined, id: string, limit: number, offset: number) {
    return this.read(token, async (manager, organizationId) => {
      const provider = await this.provider(manager, organizationId, id),
        page = await models.fixedPage(manager, id, limit, offset);
      return { ...page, items: page.items.map((row) => this.fixedView(row, provider.connection)) };
    });
  }

  saveConfiguration(token: string | undefined, value: unknown, requestId: string) {
    const input = fields(value, ['provider_instance_id', 'model_id', 'origin', 'parameters']);
    const selection = selectionInput({ provider_instance_id: input.provider_instance_id, model_id: input.model_id });
    const origin = enumeration(input.origin, ['online', 'manual']),
      parameters = parseParameters(input.parameters);
    return this.write(
      token,
      requestId,
      'model_configuration_saved',
      async (manager, organizationId, invalidate) => {
        const provider = await this.provider(manager, organizationId, selection.provider_instance_id),
          previous = await models.fixed(manager, selection);
        if (previous && previous.origin !== origin) invalid();
        if (
          !previous &&
          origin === 'online' &&
          !provider.model_catalog?.some((item) => item.model_id === selection.model_id)
        )
          throw new IdentityError(404, 'MODEL_NOT_FOUND');
        if (
          !validParameters(parameters, true) ||
          (resolveProtocol(provider.connection) === 'open_ai_chat_completions' &&
            parameters.tool_image_projection === 'native_tool_result') ||
          (parameters.server_compaction_enabled && resolveProtocol(provider.connection) !== 'open_ai_responses')
        )
          throw new IdentityError(400, 'MODEL_CONFIGURATION_INVALID');
        const policy = await models.policy(manager, organizationId);
        const before = await this.evaluate(manager, provider, selection);
        await models.saveFixed(manager, selection, origin, parameters);
        const after = await this.evaluate(manager, provider, selection);
        if (policy.allowed_models.some((item) => sameSelection(item, selection)) && this.executionChanged(before, after)) {
          await models.renewHistory(manager, provider);
          invalidate(provider.provider_instance_id);
        }
        return this.configurationIn(manager, organizationId, selection, origin);
      },
      selection,
    );
  }

  reset(token: string | undefined, value: unknown, requestId: string) {
    const selection = selectionInput(value);
    return this.write(
      token,
      requestId,
      'model_configuration_reset',
      async (manager, organizationId, invalidate) => {
        const provider = await this.provider(manager, organizationId, selection.provider_instance_id);
        const policy = await models.policy(manager, organizationId);
        const before = await this.evaluate(manager, provider, selection);
        await models.removeFixed(manager, selection);
        const after = await this.evaluate(manager, provider, selection);
        if (policy.allowed_models.some((item) => sameSelection(item, selection)) && this.executionChanged(before, after)) {
          await models.renewHistory(manager, provider);
          invalidate(provider.provider_instance_id);
        }
      },
      selection,
    );
  }

  private async evaluate(
    manager: EntityManager,
    provider: ProviderEntity | null,
    selection: ModelSelection,
    templates?: Awaited<ReturnType<ModelTemplates['read']>>,
  ): Promise<{ availability: ModelAvailability; configuration: ModelConfiguration | null }> {
    const configuration = provider
      ? await this.resolveConfiguration(manager, provider, selection, 'online', templates)
      : null;
    let reason: string | null = null;
    if (!provider) reason = 'provider_deleted';
    else if (!configuration) reason = 'model_absent';
    else if (configuration.requires_configuration) reason = 'invalid_parameters';
    return {
      configuration,
      availability: {
        ...selection,
        state: reason ? 'unavailable' : 'ready',
        reason,
        display_name: provider?.model_catalog?.find((item) => item.model_id === selection.model_id)?.display_name ?? null,
        origin: configuration?.origin ?? null,
      },
    };
  }

  private executionChanged(before: Awaited<ReturnType<ModelsService['evaluate']>>, after: Awaited<ReturnType<ModelsService['evaluate']>>) {
    return before.availability.state !== after.availability.state ||
      configurationIdentity(before.configuration?.parameters) !== configurationIdentity(after.configuration?.parameters);
  }

  private async settingsIn(manager: EntityManager, organizationId: string): Promise<ModelSettings> {
    const policy = await models.policy(manager, organizationId);
    const availability: ModelAvailability[] = [];
    for (const selection of policy.allowed_models) {
      const provider = await models.provider(manager, organizationId, selection.provider_instance_id);
      availability.push((await this.evaluate(manager, provider, selection)).availability);
    }
    return { policy, public_origin: null, availability };
  }

  settings(token?: string) {
    return this.identity.write(async manager => {
      const session = await this.identity.scope(token);
      if (session.scope.kind === 'system') {
        await this.identity.requireAdmin(manager, token);
        return this.platformSettings(manager);
      }
      const { organizationId } = await this.identity.requireOrganizationAdmin(manager, token);
      return this.settingsIn(manager, organizationId);
    });
  }

  private async platformSettings(manager: EntityManager): Promise<ModelSettings> {
    return { public_origin: (await models.publicOrigin(manager)) ?? this.origin ?? null, policy: null, availability: [] };
  }

  saveSettings(token: string | undefined, value: unknown, requestId: string) {
    const input = fields(value, [], ['policy', 'public_origin']);
    if (Object.keys(input).length !== 1) invalid();
    if (Object.hasOwn(input, 'public_origin')) {
      const origin = input.public_origin === null ? null : parsePublicOrigin(input.public_origin);
      return this.identity.write(async manager => {
        const actor = await this.identity.requireAdmin(manager, token);
        await models.publicOriginSave(manager, origin);
        await this.identity.audit(manager, 'model_public_origin_changed', requestId, actor.id, undefined, null,
          { public_origin: origin });
        return this.platformSettings(manager);
      });
    }
    const next = policyInput(input.policy);
    return this.write(
      token,
      requestId,
      'model_policy_changed',
      async (manager, organizationId, invalidate) => {
        // 校验、替换、续接周期和审计共用当前事务；旧草稿不覆盖其他管理员的提交。
        const previous = await models.policy(manager, organizationId);
        if (previous.revision !== next.revision) throw new IdentityError(409, 'MODEL_POLICY_CONFLICT');
        const previousKeys = new Set(previous.allowed_models.map(selectionKey));
        for (const selection of next.allowed_models) {
          if (sameSelection(next.default_model, selection) || !previousKeys.has(selectionKey(selection))) {
            const configuration = await this.configurationIn(manager, organizationId, selection, 'online');
            if (configuration.requires_configuration) throw new IdentityError(400, 'MODEL_CONFIGURATION_INVALID');
          }
        }

        // 撤销时更新 Provider 周期，重新授权不会复活旧续接。只扩张授权时沿用现有链。
        const nextKeys = new Set(effectiveModels(next).map(selectionKey));
        const changed = new Set(effectiveModels(previous)
          .filter((item) => !nextKeys.has(selectionKey(item)))
          .map((item) => item.provider_instance_id));
        if (!sameSelection(previous.default_model, next.default_model)) {
          for (const item of [previous.default_model, next.default_model]) {
            if (item) changed.add(item.provider_instance_id);
          }
        }
        for (const id of changed) {
          const provider = await models.provider(manager, organizationId, id);
          if (!provider) continue;
          await models.renewHistory(manager, provider);
          invalidate(id);
        }
        await models.savePolicy(manager, organizationId, { ...next, revision: randomUUID() });
        return this.settingsIn(manager, organizationId);
      },
      { default_model: next.default_model, allow_member_selection: next.allow_member_selection, model_count: next.allowed_models.length },
    );
  }

  async reload(token: string | undefined, requestId: string) {
    await this.identity.requireAdmin(this.manager, token);
    let entries: Awaited<ReturnType<ModelTemplates['read']>>;
    try {
      entries = await this.templates.read();
    } catch {
      this.templates.failed();
      throw new IdentityError(400, 'MODEL_TEMPLATES_INVALID');
    }
    const invalidated = new Set<string>();
    return this.identity
      .write(
        async (manager) => {
          const actor = await this.identity.requireAdmin(manager, token);
          for (const { id: organizationId } of await organizations.list(manager)) {
            const policy = await models.policy(manager, organizationId);
            for (const selection of policy.allowed_models) {
              const id = selection.provider_instance_id;
              if (invalidated.has(id)) continue;
              const provider = await models.provider(manager, organizationId, id);
              if (!provider) continue;
              const before = await this.evaluate(manager, provider, selection);
              if (before.configuration?.source === 'fixed') continue;
              const after = await this.evaluate(manager, provider, selection, entries);
              if (this.executionChanged(before, after)) {
                await models.renewHistory(manager, provider);
                invalidated.add(id);
              }
            }
          }
          await this.identity.audit(manager, 'model_templates_reloaded', requestId, actor.id, undefined, null, {
            count: entries.length,
          });
        },
        () => {
          this.templates.publish(entries);
          this.invalidate(invalidated);
        },
      )
      .then(() => this.templates.status());
  }

  /** 准入与身份变更共用串行门；返回当前秘密连接给内部传输，绝不作为公共 DTO 返回。 */
  async admit(
    key: string,
    selected: ModelSelection,
    protocol: Protocol,
    adminTest = false,
    history?: { request: RequestModel; binding?: string; operation?: 'generation' | 'compact' | 'input_tokens' },
  ) {
    return this.identity.write(async (manager) => {
      let subject: Awaited<ReturnType<IdentityService['llmIdentity']>> | undefined;
      let organizationId: string;
      if (adminTest) ({ organizationId } = await this.identity.requireOrganizationAdmin(manager, key));
      else {
        try {
          subject = await this.identity.llmIdentity(key);
        } catch (error) {
          if (error instanceof IdentityError && error.status === 401) throw new ControlError('llm_key_invalid');
          throw error;
        }
        organizationId = subject.scope.organization_id!;
        const policy = await models.policy(manager, organizationId);
        const eligible = effectiveModels(policy);
        if (eligible.length === 0) throw new ControlError('managed_model_unavailable');
        if (!eligible.some((item) => sameSelection(item, selected))) {
          throw new ControlError('managed_model_configuration_stale');
        }
      }
      const provider = await models.provider(manager, organizationId, selected.provider_instance_id);
      if (!provider) {
        if (adminTest) throw new IdentityError(404, 'MODEL_NOT_FOUND');
        throw new ControlError('managed_model_unavailable');
      }
      if (resolveProtocol(provider.connection) !== protocol)
        throw new ControlError('managed_model_configuration_stale');
      if (adminTest && !(await models.fixed(manager, selected)))
        throw new IdentityError(400, 'MODEL_CONFIGURATION_INVALID');
      const configuration = await this.resolveConfiguration(manager, provider, selected, 'online');
      if (!configuration) throw new ControlError('managed_model_unavailable');
      if (configuration.requires_configuration) throw new ControlError('managed_model_unavailable');
      if (
        history &&
        protocol === 'open_ai_chat_completions' &&
        (history.request.store === true ||
          history.request.previous_response_id != null ||
          history.binding !== undefined)
      )
        throw new ControlError('llm_request_invalid');
      if (history && protocol === 'open_ai_responses') {
        const { request, binding } = history;
        if (request.external_history) throw new ControlError('llm_request_invalid');
        const operation = history.operation ?? 'generation';
        const expected = subject ? historyBinding(provider, configuration, subject.center_id, subject.user.id) : null;
        if (operation === 'generation' && (request.store === undefined || (request.previous_response_id != null && request.store !== true)))
          throw new ControlError('llm_request_invalid');
        if (operation !== 'generation') {
          if (adminTest || (!request.streaming_compaction && (request.store !== undefined || request.previous_response_id !== undefined || request.stream)))
            throw new ControlError('llm_request_invalid');
          const parameters = configuration.parameters;
          if (operation === 'compact' && (!parameters.server_compaction_enabled || parameters.server_compaction_support !== 'supported'))
            throw new ControlError('managed_model_configuration_stale');
          if (operation === 'input_tokens' && parameters.input_token_count_support !== 'supported')
            throw new ControlError('managed_model_configuration_stale');
          if (expected && binding !== expected) throw new ControlError('managed_model_configuration_stale');
        }
        if (adminTest && (request.store !== false || binding !== undefined))
          throw new ControlError('llm_request_invalid');
        if (
          (binding !== undefined && (!expected || binding !== expected)) ||
          (request.store &&
            (!provider.connection.server_history_enabled || !expected || binding !== expected || serverHistorySupport(provider.connection) !== 'supported'))
        )
          throw new ControlError('managed_model_configuration_stale');
      }
      return { provider, configuration };
    });
  }

  /** 下发与代理准入读取同一组织策略。只有单项缺失可降级，数据库等集合读取错误原样返回。 */
  async runtime(token?: string): Promise<RuntimeModelConfiguration> {
    return this.identity.write(async (manager) => {
      const { organizationId } = await this.identity.requireOrganizationMember(manager, token);
      const identity = await this.identity.me(token);
      const policy = await models.policy(manager, organizationId);
      const eligible = effectiveModels(policy);
      const origin = (await models.publicOrigin(manager)) ?? this.origin;
      if (eligible.length && !origin) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');

      const providers = new Map<string, { view: RuntimeModelProvider; provider: ProviderEntity | null }>();
      for (const selection of eligible) {
        const id = selection.provider_instance_id;
        let group = providers.get(id);
        if (!group) {
          const provider = await models.provider(manager, organizationId, id);
          group = {
            provider,
            view: {
              provider_instance_id: id,
              endpoint: `${origin}/api/llm/providers/${id}/v1`,
              provider_type: provider?.connection.provider_type ?? null,
              provider_display_name: provider?.connection.display_name ?? null,
              protocol: provider ? resolveProtocol(provider.connection) : null,
              server_history_enabled: provider?.connection.server_history_enabled ?? false,
              server_history_support: provider ? serverHistorySupport(provider.connection) : 'unknown',
              models: [],
            },
          };
          providers.set(id, group);
        }
        const { provider, view } = group;
        const { availability, configuration } = await this.evaluate(manager, provider, selection);
        const ready = availability.state === 'ready' ? configuration : null;
        view.models.push({
          model_id: selection.model_id,
          state: availability.state,
          reason: availability.reason,
          parameters: ready?.parameters ?? null,
          server_history_binding: provider && ready ? historyBinding(provider, ready, identity.center_id, identity.user.id) : null,
        });
      }
      return {
        center_id: identity.center_id,
        organization_id: organizationId,
        allow_member_selection: policy.allow_member_selection,
        default_model: policy.default_model,
        providers: [...providers.values()].map(({ view }) => view),
      };
    });
  }
}
