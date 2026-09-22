import { ControlError } from '../llm/errors.js';
import type { Protocol } from '../contracts/models.js';
import { randomUUID } from 'node:crypto';
import type { DataSource, EntityManager } from 'typeorm';
import type {
  ModelConfiguration,
  ModelSelection,
  ModelSettings,
  ProviderConnection,
  ProviderSummary,
  SaveProvider,
} from '../contracts/models.js';
import { IdentityService } from '../identity/service.js';
import { IdentityError } from '../identity/errors.js';
import { fields, invalid } from '../validation.js';
import { ModelTemplates } from './templates.js';
import { models } from './repository.js';
import type { FixedConfigEntity, ProviderEntity } from './entity.js';
import {
  emptyParameters,
  enumeration,
  executable,
  mergeParameters,
  parseConnection,
  parseParameters,
  resolveProtocol,
  text,
  uuid,
  validParameters,
} from './parameters.js';
import { discover } from './discovery.js';

const summary = (row: ProviderEntity): ProviderSummary => ({
  provider_instance_id: row.provider_instance_id,
  connection: row.connection,
  has_api_key: Boolean(row.api_key),
});

const selectionInput = (value: unknown): ModelSelection => {
  const input = fields(value, ['provider_instance_id', 'model_id']);
  return { provider_instance_id: uuid(input.provider_instance_id), model_id: text(input.model_id, 1024) };
};

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

  async onModuleDestroy(): Promise<void> {
    this.shutdown.abort();
    await Promise.allSettled(this.refreshes.values());
  }

  private async provider(manager: EntityManager, id: string) {
    const row = await models.provider(manager, uuid(id));
    if (!row) throw new IdentityError(404, 'MODEL_NOT_FOUND');
    return row;
  }

  private read<T>(token: string | undefined, work: (manager: EntityManager) => Promise<T>): Promise<T> {
    return this.identity.write(async (manager) => {
      await this.identity.requireAdmin(manager, token);
      return work(manager);
    });
  }

  private write<T>(
    token: string | undefined,
    requestId: string,
    action: string,
    work: (manager: EntityManager) => Promise<T>,
    details: object = {},
  ): Promise<T> {
    return this.identity.write(async (manager) => {
      const actor = await this.identity.requireAdmin(manager, token);
      const result = await work(manager);
      await this.identity.audit(manager, action, requestId, actor.id, undefined, null, details);
      return result;
    });
  }

  async list(token?: string) {
    return this.read(token, async (manager) => {
      // 列表明确只选择凭据是否存在，不能将秘密 Entity 序列化出站。
      const rows = await manager
        .getRepository('providers')
        .createQueryBuilder('p')
        .select(['p.provider_instance_id', 'p.connection'])
        .addSelect("p.api_key <> ''", 'has_api_key')
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
    return this.read(token, async (manager) => summary(await this.provider(manager, id)));
  }

  save(token: string | undefined, id: string | undefined, value: unknown, requestId: string) {
    const input = parseSave(value),
      providerId = id === undefined ? randomUUID() : uuid(id);
    return this.write(
      token,
      requestId,
      id === undefined ? 'model_provider_created' : 'model_provider_updated',
      async (manager) => {
        const previous = id === undefined ? null : await this.provider(manager, providerId);
        let key = previous?.api_key ?? '';
        if (input.credential.mode === 'replace') key = input.credential.value!;
        if (input.credential.mode === 'clear') key = '';
        if (input.connection.provider_type === 'dashscope_api' && key.startsWith('sk-sp-')) invalid();
        if (!previous) await models.insert(manager, providerId, input.connection, key);
        else {
          const changed =
            discoveryIdentity(previous.connection, previous.api_key) !== discoveryIdentity(input.connection, key);
          await models.update(manager, {
            ...previous,
            connection: input.connection,
            api_key: key,
            catalog_connection_changed:
              previous.catalog_connection_changed || (previous.model_catalog !== null && changed),
          });
        }
        return summary(await this.provider(manager, providerId));
      },
      { provider_instance_id: providerId },
    );
  }

  private async usageIn(manager: EntityManager, id: string) {
    await this.provider(manager, id);
    const selected = await models.settings(manager);
    return {
      fixed_config_count: await models.fixedCount(manager, id),
      default_model: selected?.provider_instance_id === id,
    };
  }

  usage(token: string | undefined, id: string) {
    return this.read(token, (manager) => this.usageIn(manager, id));
  }

  remove(token: string | undefined, id: string, requestId: string) {
    return this.write(
      token,
      requestId,
      'model_provider_deleted',
      async (manager) => {
        const usage = await this.usageIn(manager, id);
        await models.remove(manager, id);
        return usage;
      },
      { provider_instance_id: uuid(id) },
    );
  }

  catalog(token: string | undefined, id: string) {
    return this.read(token, async (manager) => this.catalogView(await this.provider(manager, id)));
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
    await this.identity.requireAdmin(this.manager, token);
    const existing = this.refreshes.get(id);
    if (existing) {
      await existing;
      return this.catalog(token, id);
    }
    if (this.shutdown.signal.aborted) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    const pending = (async () => {
      const captured = await this.provider(this.manager, id);
      const catalog = await discover(captured.connection, captured.api_key, this.shutdown.signal);
      return this.write(
        token,
        requestId,
        'model_catalog_refreshed',
        async (manager) => {
          const current = await this.provider(manager, id);
          if (this.shutdown.signal.aborted) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
          if (
            discoveryIdentity(current.connection, current.api_key) !==
            discoveryIdentity(captured.connection, captured.api_key)
          )
            throw new IdentityError(409, 'MODEL_CONFIGURATION_CHANGED');
          await models.catalog(manager, id, catalog);
          return this.catalogView(await this.provider(manager, id));
        },
        { provider_instance_id: id, model_count: catalog.length },
      );
    })();
    this.refreshes.set(id, pending);
    try {
      return await pending;
    } finally {
      if (this.refreshes.get(id) === pending) this.refreshes.delete(id);
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
    selection: ModelSelection,
    origin: 'online' | 'manual',
  ): Promise<ModelConfiguration> {
    const provider = await this.provider(manager, selection.provider_instance_id),
      fixed = await models.fixed(manager, selection);
    if (fixed) return this.fixedView(fixed, provider.connection);
    const online = provider.model_catalog?.find((model) => model.model_id === selection.model_id);
    if (!online && origin !== 'manual') throw new IdentityError(404, 'MODEL_NOT_FOUND');
    const template = this.templates.lookup(provider.connection, selection.model_id);
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
    return this.read(token, (manager) => this.configurationIn(manager, selection, origin));
  }

  fixedPage(token: string | undefined, id: string, limit: number, offset: number) {
    return this.read(token, async (manager) => {
      const provider = await this.provider(manager, id),
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
      async (manager) => {
        const provider = await this.provider(manager, selection.provider_instance_id),
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
            parameters.tool_image_projection === 'native_tool_result')
        )
          throw new IdentityError(400, 'MODEL_CONFIGURATION_INVALID');
        await models.saveFixed(manager, selection, origin, parameters);
        return this.configurationIn(manager, selection, origin);
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
      async (manager) => {
        await this.provider(manager, selection.provider_instance_id);
        await models.removeFixed(manager, selection);
      },
      selection,
    );
  }

  private async settingsIn(manager: EntityManager): Promise<ModelSettings> {
    const selection = await models.settings(manager);
    if (!selection) return { default_model: null, state: 'unavailable', reason: 'not_selected', configuration: null };
    const provider = await models.provider(manager, selection.provider_instance_id);
    if (!provider)
      return { default_model: selection, state: 'unavailable', reason: 'provider_deleted', configuration: null };
    let configuration: ModelConfiguration;
    try {
      configuration = await this.configurationIn(manager, selection, 'online');
    } catch (error) {
      if (!(error instanceof IdentityError) || error.code !== 'MODEL_NOT_FOUND') throw error;
      return { default_model: selection, state: 'unavailable', reason: 'model_absent', configuration: null };
    }
    return {
      default_model: selection,
      state: configuration.requires_configuration ? 'unavailable' : 'ready',
      reason: configuration.requires_configuration ? 'invalid_parameters' : null,
      configuration,
    };
  }

  settings(token?: string) {
    return this.read(token, (manager) => this.settingsIn(manager));
  }

  setDefault(token: string | undefined, value: unknown, requestId: string) {
    const input = fields(value, ['default_model']),
      selection = input.default_model === null ? null : selectionInput(input.default_model);
    return this.write(
      token,
      requestId,
      'model_default_changed',
      async (manager) => {
        if (selection && (await this.configurationIn(manager, selection, 'online')).requires_configuration)
          throw new IdentityError(400, 'MODEL_CONFIGURATION_INVALID');
        await models.settingsSave(manager, selection);
        return this.settingsIn(manager);
      },
      { default_model: selection },
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
    return this.identity
      .write(
        async (manager) => {
          const actor = await this.identity.requireAdmin(manager, token);
          await this.identity.audit(manager, 'model_templates_reloaded', requestId, actor.id, undefined, null, {
            count: entries.length,
          });
        },
        () => this.templates.publish(entries),
      )
      .then(() => this.templates.status());
  }

  /** 准入与身份变更共用串行门；返回当前秘密连接给内部传输，绝不作为公共 DTO 返回。 */
  async admit(key: string, selected: ModelSelection, protocol: Protocol, adminTest = false) {
    return this.identity.write(async (manager) => {
      if (adminTest) await this.identity.requireAdmin(manager, key);
      else {
        try {
          await this.identity.llmIdentity(key);
        } catch (error) {
          if (error instanceof IdentityError && error.status === 401) throw new ControlError('llm_key_invalid');
          throw error;
        }
        const settings = await this.settingsIn(manager);
        if (settings.state !== 'ready') throw new ControlError('managed_model_unavailable');
        if (
          settings.default_model?.provider_instance_id !== selected.provider_instance_id ||
          settings.default_model.model_id !== selected.model_id
        ) {
          throw new ControlError('managed_model_configuration_stale');
        }
      }
      const provider = await this.provider(manager, selected.provider_instance_id);
      if (resolveProtocol(provider.connection) !== protocol)
        throw new ControlError('managed_model_configuration_stale');
      if (adminTest && !(await models.fixed(manager, selected)))
        throw new IdentityError(400, 'MODEL_CONFIGURATION_INVALID');
      const configuration = await this.configurationIn(manager, selected, 'online');
      if (configuration.requires_configuration) throw new ControlError('managed_model_unavailable');
      return { provider, configuration };
    });
  }

  async runtime(token?: string) {
    return this.identity.write(async (manager) => {
      const identity = await this.identity.me(token),
        settings = await this.settingsIn(manager);
      if (settings.state === 'unavailable')
        return { state: 'unavailable' as const, center_id: identity.center_id, reason: settings.reason! };
      const selected = settings.default_model!,
        provider = await this.provider(manager, selected.provider_instance_id);
      if (!this.origin) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
      return {
        state: 'ready' as const,
        center_id: identity.center_id,
        endpoint: `${this.origin}/api/llm/providers/${selected.provider_instance_id}/v1`,
        provider_type: provider.connection.provider_type,
        provider_display_name: provider.connection.display_name,
        protocol: resolveProtocol(provider.connection),
        model_id: selected.model_id,
        parameters: settings.configuration!.parameters,
      };
    });
  }
}
