import { readFileSync } from 'node:fs';
import { expect, it, vi } from 'vitest';
import type { EntityManager } from 'typeorm';
import { IdentityService } from '../../dist/identity/service.js';
import { ModelsService } from '../../dist/models/service.js';
import { ModelTemplates } from '../../dist/models/templates.js';
import { models } from '../../dist/models/repository.js';
import { UserEntity } from '../../dist/users/entity.js';
import { configurationIdentity, historyBinding, historyConnectionIdentity, serverHistorySupport } from '../../dist/models/history.js';
import { parseConnection, emptyParameters } from '../../dist/models/parameters.js';
import { ProviderEntity } from '../../dist/models/entity.js';
import { requestModel } from '../../dist/llm/body.js';

const fixtures = JSON.parse(readFileSync('../../packages/assistant-protocol/fixtures/server-history.json', 'utf8'));
it('企业实际上游能力与 Rust/共享 TS 夹具一致，未知能力不能启用', () => {
  for (const fixture of fixtures) {
    const connection = { server_history_enabled: false, server_history_support_override: null, ...fixture.connection };
    expect(serverHistorySupport(connection), fixture.name).toBe(fixture.expected);
    if (fixture.expected !== 'supported') expect(() => parseConnection({ ...connection, server_history_enabled: true })).toThrow();
  }
  expect(() => parseConnection(fixtures[0].connection)).toThrow();
  expect(() => parseConnection({ ...fixtures[0].connection, server_history_enabled: true, server_history_support_override: 'unknown' })).toThrow();
});

it('不透明绑定稳定、隔离主体和真实连接配置，参数键序不构成变化', () => {
  const provider = Object.assign(new ProviderEntity(), {
    provider_instance_id: '00000000-0000-0000-0000-000000000001',
    connection: { ...fixtures[0].connection, server_history_enabled: true, server_history_support_override: null },
    api_key: 'artificial-secret', server_history_key: 'artificial-cycle',
  });
  const configuration = { provider_instance_id: provider.provider_instance_id, model_id: 'model',
    origin: 'manual' as const, parameters: emptyParameters(), source: 'fixed', field_sources: {},
    updated_at_ms: null, template_document: null, template_checked_on: null, requires_configuration: false };
  const binding = historyBinding(provider, configuration, 'center', 1);
  expect(binding).toMatch(/^[a-f0-9]{64}$/);
  expect(historyBinding(provider, configuration, 'center', 1)).toBe(binding);
  expect(historyBinding(provider, configuration, 'center', 2)).not.toBe(binding);
  expect(historyBinding(provider, configuration, 'another-center', 1)).not.toBe(binding);
  provider.api_key = 'new-secret';
  expect(historyBinding(provider, configuration, 'center', 1)).not.toBe(binding);
  provider.connection.server_history_enabled = false;
  expect(historyBinding(provider, configuration, 'center', 1)).toBeNull();
  expect(configurationIdentity({ b: 2, a: 1 })).toBe(configurationIdentity({ a: 1, b: 2 }));
});

it('请求准入拒绝重复转义历史键和错误类型，解析不改写原字节', () => {
  const original = Buffer.from('{"model":"m", "store":true, "previous_response_id":"resp_1", "n":9007199254740993}');
  const retained = Buffer.from(original);
  expect(requestModel(original)).toEqual({ model: 'm', stream: false, store: true, previous_response_id: 'resp_1' });
  expect(original).toEqual(retained);
  for (const tail of [
    '"store":false,"st\\u006fre":true',
    '"previous_response_id":"a","previous_response_\\u0069d":"b"',
    '"store":null', '"previous_response_id":1', '"previous_response_id":""',
  ]) expect(() => requestModel(Buffer.from(`{"model":"m",${tail}}`))).toThrow();
});


it('WebSocket 配置不继承其他服务商 HTTP store 能力，默认 HTTP 保留原绑定', () => {
  const legacy = { ...fixtures[0].connection, server_history_enabled: false, server_history_support_override: null };
  expect(parseConnection(legacy).responses_transport).toBe('http');
  expect(historyConnectionIdentity(legacy, 'key')).toBe(historyConnectionIdentity({ ...legacy, responses_transport: 'http' }, 'key'));
  expect(historyConnectionIdentity({ ...legacy, responses_transport: 'websocket' }, 'key')).not.toBe(historyConnectionIdentity(legacy, 'key'));
  const custom = { ...legacy, responses_transport: 'websocket', endpoint: 'https://proxy.example/v1' };
  expect(serverHistorySupport(custom)).toBe('unknown');
  expect(() => parseConnection({ ...custom, server_history_enabled: true })).toThrow();
  expect(parseConnection({ ...custom, server_history_enabled: true, server_history_support_override: 'supported' }).responses_transport).toBe('websocket');
  expect(() => parseConnection({ ...custom, protocol_preference: 'chat_completions' })).toThrow();
  expect(() => parseConnection({ ...legacy, responses_transport: null })).toThrow();
});


it('旧 Admin 缺传输字段不能覆盖 WebSocket，显式切换才保存并换绑定', async () => {
  const identity = new IdentityService();
  const service = new ModelsService(identity, undefined, new ModelTemplates());
  const row = Object.assign(new ProviderEntity(), {
    provider_instance_id: '00000000-0000-4000-8000-000000000001', api_key: 'fixture', server_history_key: 'original',
    connection: parseConnection({ ...fixtures[0].connection, server_history_enabled: true,
      server_history_support_override: 'supported', responses_transport: 'websocket' }),
  });
  // Manager 和仓储方法仅为内存夹具；不初始化 DataSource、不执行 SQL。
  vi.spyOn(identity, 'write').mockImplementation(async work => work({} as EntityManager));
  vi.spyOn(identity, 'requireAdmin').mockResolvedValue(Object.assign(new UserEntity(), { id: 1 }));
  vi.spyOn(identity, 'audit').mockResolvedValue();
  vi.spyOn(models, 'provider').mockResolvedValue(row);
  const update = vi.spyOn(models, 'update').mockImplementation(async (_manager, next) => {
    Object.assign(row, next);
    return { affected: 1, raw: [], generatedMaps: [] };
  });
  try {
    const { responses_transport: _transport, ...oldPage } = row.connection;
    await expect(service.save('token', row.provider_instance_id, { connection: oldPage, credential: { mode: 'unchanged' } }, 'request')).rejects.toThrow();
    expect(update).not.toHaveBeenCalled();
    await service.save('token', row.provider_instance_id, { connection: { ...oldPage, responses_transport: 'http' }, credential: { mode: 'unchanged' } }, 'request');
    expect(row.connection.responses_transport).toBe('http');
    expect(row.server_history_key).not.toBe('original');
  } finally {
    vi.restoreAllMocks();
    await service.onModuleDestroy();
  }
});
