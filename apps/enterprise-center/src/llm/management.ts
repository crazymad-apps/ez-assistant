import type { DataSource } from 'typeorm';
import { IdentityService } from '../identity/service.js';
import { IdentityError } from '../identity/errors.js';
import { fields, invalid, queryFields, recordId } from '../validation.js';
import { enumeration, text, uuid, resolveProtocol } from '../models/parameters.js';
import type { ModelsService } from '../models/service.js';
import { CallEntity, RecordingEntity } from './entity.js';
import { noSnapshot } from './snapshots.js';
import type { Side } from './snapshots.js';
import { outcomes } from '../contracts/calls.js';
import type { CallDetails, CallPage, RecordingSettings, SnapshotContent, ModelTestResult } from '../contracts/calls.js';
import type { ModelProxy } from './proxy.js';
import type { CallRecording } from './records.js';

function snapshotView(row: CallEntity, side: Side) {
  return {
    state: row[`${side}_snapshot_state`],
    bytes: row[`${side}_bytes`] === null ? null : Number(row[`${side}_bytes`]),
    sha256: row[`${side}_sha256`],
    reason: row[`${side}_snapshot_reason`],
  };
}

function view(row: CallEntity): CallDetails {
  return {
    id: row.id,
    user_id: row.user_id,
    username: row.username,
    provider_instance_id: row.provider_instance_id,
    provider_name: row.provider_name,
    model_id: row.model_id,
    kind: row.kind,
    protocol: row.protocol,
    started_at: row.started_at.toISOString(),
    ended_at: row.ended_at?.toISOString() ?? null,
    http_status: row.http_status,
    reason: row.reason,
    outcome: row.outcome,
    request: snapshotView(row, 'request'),
    response: snapshotView(row, 'response'),
  };
}

export class CallManagement {
  constructor(
    private readonly identities: IdentityService,
    private readonly source: DataSource | undefined,
    private readonly proxy: ModelProxy,
    private readonly models: ModelsService,
  ) {}

  private get manager() {
    if (!this.source) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    return this.source.manager;
  }

  async list(token: string | undefined, value: unknown): Promise<CallPage> {
    await this.identities.requireAdmin(this.manager, token);
    const input = queryFields(value, [
      'id',
      'user_id',
      'provider_instance_id',
      'model_id',
      'outcome',
      'kind',
      'from',
      'to',
      'limit',
      'offset',
    ]);

    const integer = (name: string, fallback: number, min: number, max: number) => {
      if (input[name] === undefined) return fallback;
      if (!/^(0|[1-9][0-9]*)$/.test(input[name]!)) invalid();
      const value = Number(input[name]);
      if (!Number.isSafeInteger(value) || value < min || value > max) invalid();
      return value;
    };

    const limit = integer('limit', 50, 1, 200),
      offset = integer('offset', 0, 0, 1000000);
    const query = this.manager.getRepository(CallEntity).createQueryBuilder('c');
    for (const key of ['id', 'provider_instance_id'])
      if (input[key] !== undefined) query.andWhere(`c.${key} = :${key}`, { [key]: uuid(input[key]) });
    if (input.user_id !== undefined) query.andWhere('c.user_id = :user', { user: recordId(input.user_id) });
    if (input.model_id !== undefined) query.andWhere('c.model_id = :model', { model: text(input.model_id, 1024) });
    if (input.outcome !== undefined)
      query.andWhere('c.outcome = :outcome', { outcome: enumeration(input.outcome, outcomes) });
    if (input.kind !== undefined)
      query.andWhere('c.kind = :kind', { kind: enumeration(input.kind, ['proxy', 'admin_test']) });
    for (const name of ['from', 'to'])
      if (input[name] !== undefined) {
        const value = input[name]!;
        if (
          !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,3})?Z$/.test(value) ||
          !Number.isFinite(Date.parse(value)) ||
          new Date(value).toISOString().slice(0, 19) !== value.slice(0, 19)
        )
          invalid();
        query.andWhere(`c.started_at ${name === 'from' ? '>=' : '<'} :${name}`, { [name]: value });
      }
    if (input.from && input.to && Date.parse(input.from) >= Date.parse(input.to)) invalid();
    const [rows, total] = await query
      .orderBy('c.started_at', 'DESC')
      .addOrderBy('c.id', 'DESC')
      .skip(offset)
      .take(limit)
      .getManyAndCount();
    await this.identities.requireAdmin(this.manager, token);
    return { items: rows.map(view), total, limit, offset };
  }

  private async row(id: string) {
    const row = await this.manager.getRepository(CallEntity).findOneBy({ id: uuid(id) });
    if (!row) throw new IdentityError(404, 'CALL_NOT_FOUND');
    return row;
  }

  async get(token: string | undefined, id: string): Promise<CallDetails> {
    await this.identities.requireAdmin(this.manager, token);
    return view(await this.row(id));
  }

  async settings(token?: string): Promise<RecordingSettings> {
    await this.identities.requireAdmin(this.manager, token);
    const { enabled, content_retention_days, index_retention_days } = await this.proxy.records.settings();
    return { enabled, content_retention_days, index_retention_days };
  }

  async saveSettings(token: string | undefined, value: unknown, requestId: string): Promise<RecordingSettings> {
    const input = fields(value, ['enabled', 'content_retention_days', 'index_retention_days']);
    const { enabled, content_retention_days, index_retention_days } = input;
    if (
      typeof enabled !== 'boolean' ||
      typeof content_retention_days !== 'number' ||
      typeof index_retention_days !== 'number' ||
      !Number.isInteger(content_retention_days) ||
      !Number.isInteger(index_retention_days) ||
      content_retention_days < 1 ||
      index_retention_days < content_retention_days ||
      index_retention_days > 3650
    )
      invalid();
    await this.identities.requireAdmin(this.manager, token);
    if (enabled) {
      try {
        await this.proxy.records.files.validate();
      } catch {
        throw new IdentityError(400, 'SNAPSHOT_ROOT_UNAVAILABLE');
      }
    }
    const settings = { enabled, content_retention_days, index_retention_days };
    await this.identities.write(async (manager) => {
      const user = await this.identities.requireAdmin(manager, token);
      await manager.getRepository(RecordingEntity).update({ singleton: true }, settings);
      await this.identities.audit(
        manager,
        'llm_recording_settings_changed',
        requestId,
        user.id,
        undefined,
        null,
        settings,
      );
    });
    return settings;
  }

  async snapshot(token: string | undefined, id: string, side: Side, requestId: string): Promise<SnapshotContent> {
    await this.identities.requireAdmin(this.manager, token);
    const row = await this.row(id);
    let result: SnapshotContent = { ...snapshotView(row, side), text: null };
    if (result.state === 'complete' || result.state === 'partial') {
      const found = await this.proxy.records.files.read(row.id, row.started_at, side);
      if (
        !found ||
        found.result.sha256 !== row[`${side}_sha256`] ||
        String(found.result.bytes) !== row[`${side}_bytes`]
      ) {
        const broken = noSnapshot(found ? 'failed' : 'missing', found ? 'integrity_mismatch' : 'file_missing');
        await this.proxy.records.snapshot(row.id, side, broken);
        result = { state: broken.state, bytes: null, sha256: null, reason: broken.reason, text: null };
      } else {
        try {
          result.text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(found.body);
        } catch {
          const broken = noSnapshot('failed', 'invalid_utf8');
          await this.proxy.records.snapshot(row.id, side, broken);
          result = { state: broken.state, bytes: null, sha256: null, reason: broken.reason, text: null };
        }
      }
    }
    await this.identities.write(async (manager) => {
      const user = await this.identities.requireAdmin(manager, token);
      await this.identities.audit(manager, 'llm_snapshot_viewed', requestId, user.id, undefined, null, {
        call_id: row.id,
        side,
      });
    });
    return result;
  }

  async test(token: string | undefined, value: unknown, signal: AbortSignal): Promise<ModelTestResult> {
    return this.proxy.run(async () => {
      const cancelled = new AbortController();
      const timer = setTimeout(() => cancelled.abort(), 30000);
      try {
        const input = fields(value, ['provider_instance_id', 'model_id']);
        const selection = {
          provider_instance_id: uuid(input.provider_instance_id),
          model_id: text(input.model_id, 1024),
        };
        const user = await this.identities.requireAdmin(this.manager, token);
        const provider = await this.models.get(token, selection.provider_instance_id);
        const protocol = resolveProtocol(provider.connection);
        const route = await this.models.admit(token!, selection, protocol, true);
        const parameters = route.configuration.parameters;
        // 仅做短文本连通性测试；输出预算受保存规格约束，不附带历史或工具，不切换协议重试。
        const maximum = parameters.max_output_tokens.state === 'known' ? parameters.max_output_tokens.value! : 1024;
        const reasoning =
          parameters.reasoning === 'supported' && ['optional', 'always'].includes(parameters.reasoning_mode);
        const reasoningMaximum =
          reasoning && parameters.reasoning_max_output_tokens.state === 'known'
            ? parameters.reasoning_max_output_tokens.value!
            : maximum;
        const output = Math.min(maximum, reasoningMaximum, 1024);
        const body: Record<string, unknown> =
          protocol === 'open_ai_responses'
            ? { model: selection.model_id, input: 'Reply with OK.', stream: false, max_output_tokens: output }
            : {
                model: selection.model_id,
                messages: [{ role: 'user', content: 'Reply with OK.' }],
                stream: false,
                max_tokens: output,
              };
        if (reasoning && protocol === 'open_ai_chat_completions') {
          const providerType = provider.connection.provider_type;
          if (
            providerType === 'deepseek' ||
            providerType === 'zhipu' ||
            (providerType === 'moonshot' && parameters.reasoning_mode === 'optional')
          )
            body.thinking = { type: 'enabled' };
          if (providerType === 'dashscope_api' || providerType === 'dashscope_plan') {
            body.enable_thinking = true;
            body.preserve_thinking = true;
          }
        }
        const effort = parameters.default_reasoning_effort;
        if (effort && parameters.reasoning_efforts?.[effort]) {
          const wire = parameters.reasoning_efforts[effort]!;
          const value = /^[1-9][0-9]*$/.test(wire) ? Number(wire) : wire;
          if (protocol === 'open_ai_responses') body.reasoning = { effort: value };
          else body.reasoning_effort = value;
        }
        const chunks: Buffer[] = [];
        let bytes = 0,
          status: number | null = null;
        let recording: CallRecording | undefined;
        try {
          const result = await this.proxy.execute({
            key: token!,
            user,
            selection,
            protocol,
            body: Buffer.from(JSON.stringify(body)),
            kind: 'admin_test',
            signal: AbortSignal.any([signal, cancelled.signal]),

            admitted: (value) => {
              recording = value;
            },

            sink: {
              headers: (value) => {
                status = value;
              },

              chunk: async (chunk) => {
                bytes += chunk.length;
                if (bytes > 1024 * 1024) {
                  cancelled.abort();
                  throw new Error('response_limit');
                }
                chunks.push(chunk);
              },
            },
          });
          let connected = false;
          try {
            const value: unknown = JSON.parse(Buffer.concat(chunks).toString('utf8'));
            if (value && typeof value === 'object') {
              const output = Reflect.get(value, 'output');
              const choices = Reflect.get(value, 'choices');
              if (protocol === 'open_ai_responses') {
                connected =
                  Reflect.get(value, 'object') === 'response' &&
                  !Reflect.get(value, 'error') &&
                  Array.isArray(output) &&
                  output.some(
                    (item) =>
                      item &&
                      typeof item === 'object' &&
                      Array.isArray(item.content) &&
                      item.content.some(
                        (part: unknown) =>
                          part &&
                          typeof part === 'object' &&
                          Reflect.get(part, 'type') === 'output_text' &&
                          typeof Reflect.get(part, 'text') === 'string',
                      ),
                  );
              } else {
                connected =
                  Array.isArray(choices) &&
                  choices.some(
                    (choice) =>
                      choice &&
                      typeof choice === 'object' &&
                      choice.message &&
                      typeof choice.message.content === 'string',
                  );
              }
            }
          } catch {
            /* 格式不符只改变测试结果，不篡改实际转发 outcome。 */
          }
          connected &&= result.http_status >= 200 && result.http_status < 300;
          return { ...result, connected, reason: connected ? null : 'unexpected_response' };
        } catch (error) {
          if (!recording) throw error;
          return { call_id: recording.row.id, http_status: status, connected: false, reason: 'test_interrupted' };
        }
      } finally {
        clearTimeout(timer);
      }
    });
  }
}
