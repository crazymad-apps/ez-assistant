import { afterEach, expect, it, vi } from 'vitest';
import type { EntityManager } from 'typeorm';
import { IdentityService } from '../../dist/identity/service.js';
import { ModelsService } from '../../dist/models/service.js';
import { ModelTemplates } from '../../dist/models/templates.js';
import { models } from '../../dist/models/repository.js';
import { ModelSettingsEntity, ProviderEntity, FixedConfigEntity } from '../../dist/models/entity.js';
import { UserEntity } from '../../dist/users/entity.js';
import { emptyParameters, parsePublicOrigin } from '../../dist/models/parameters.js';
import { auditDetails } from '../../dist/audit/service.js';

afterEach(() => vi.restoreAllMocks());

it('审计只返回有效公开地址，人工写入的凭据和路径不出站', () => {
  expect(auditDetails({ public_origin: 'http://localhost:7320' })).toEqual({ public_origin: 'http://localhost:7320' });
  expect(auditDetails({ public_origin: null })).toEqual({ public_origin: null });
  expect(auditDetails({ public_origin: 'https://user:secret@center.example' })).toEqual({});
});

it('公开地址接受 localhost、域名和 IPv6，拒绝凭据、路径及非 HTTP(S) 输入', () => {
  for (const origin of ['http://localhost:7320', 'https://center.example', 'http://[::1]:7320']) {
    expect(parsePublicOrigin(` ${origin}/ `)).toBe(origin);
  }
  for (const value of ['', null, 7320, 'localhost:7320', 'file:///tmp', 'https://user:secret@center.example',
    'https://center.example/v1', 'https://center.example/?x=1', 'https://center.example/#x', 'http://localhost:7320\n']) {
    expect(() => parsePublicOrigin(value)).toThrow();
  }
});

function fixture() {
  // 仓储使用内存 Manager，无 DataSource、真实凭据或数据库操作。
  const settings = Object.assign(new ModelSettingsEntity(), {
    singleton: true, public_origin: null,
    provider_instance_id: '00000000-0000-4000-8000-000000000001', model_id: 'model',
  });
  const repository = {
    findOneByOrFail: vi.fn(async () => ({ ...settings })),
    update: vi.fn(async (_where: unknown, value: Partial<ModelSettingsEntity>) => {
      Object.assign(settings, value);
      return { affected: 1, raw: [], generatedMaps: [] };
    }),
  };
  const manager = { getRepository: () => repository } as unknown as EntityManager;
  const identity = new IdentityService();
  vi.spyOn(identity, 'write').mockImplementation(async work => work(manager));
  vi.spyOn(identity, 'requireAdmin').mockResolvedValue(Object.assign(new UserEntity(), { id: 1 }));
  vi.spyOn(identity, 'audit').mockResolvedValue();
  vi.spyOn(identity, 'me').mockResolvedValue({ center_id: 'stable-center', user: { id: 1 } } as Awaited<ReturnType<IdentityService['me']>>);
  const provider = Object.assign(new ProviderEntity(), {
    provider_instance_id: settings.provider_instance_id, server_history_key: null,
    connection: { provider_type: 'openai', protocol_preference: 'responses', server_history_enabled: false },
  });
  vi.spyOn(models, 'provider').mockResolvedValue(provider);
  vi.spyOn(models, 'fixed').mockResolvedValue(Object.assign(new FixedConfigEntity(), {
    provider_instance_id: settings.provider_instance_id, model_id: settings.model_id,
    origin: 'manual', updated_at: new Date(0), parameters: { ...emptyParameters(),
      context_window_tokens: { state: 'known', value: 8192 },
      max_output_tokens: { state: 'known', value: 1024 }, streaming: 'supported' },
  }));
  const renew = vi.spyOn(models, 'renewHistory');
  const makeService = () => new ModelsService(identity, undefined, new ModelTemplates(), 'http://127.0.0.1:7320');
  return { settings, repository, identity, renew, makeService };
}

it('独立保存地址，运行时和新服务实例读取持久值，不修改默认模型或历史绑定', async () => {
  const { settings, repository, renew, makeService } = fixture();
  const service = makeService();
  expect((await service.settings('admin')).public_origin).toBe('http://127.0.0.1:7320');
  const saved = await service.setDefault('admin', { public_origin: 'http://localhost:7320/' }, 'request');
  expect(saved.default_model?.model_id).toBe('model');
  expect(repository.update).toHaveBeenCalledExactlyOnceWith({ singleton: true }, { public_origin: 'http://localhost:7320' });
  expect(settings.model_id).toBe('model');
  expect(renew).not.toHaveBeenCalled();
  const restarted = makeService();
  expect((await restarted.runtime('user')).endpoint).toBe(`http://localhost:7320/api/llm/providers/${settings.provider_instance_id}/v1`);
  expect((await restarted.runtime('user')).center_id).toBe('stable-center');
  await restarted.setDefault('admin', { default_model: null }, 'request');
  expect(settings.public_origin).toBe('http://localhost:7320');
  await restarted.setDefault('admin', { public_origin: null }, 'request');
  expect((await restarted.settings('admin')).public_origin).toBe('http://127.0.0.1:7320');
});

it('非法地址、混合草稿和非管理员请求均不落库', async () => {
  const { repository, identity, makeService } = fixture();
  const service = makeService();
  for (const body of [{}, { public_origin: 'ftp://center.example' }, { public_origin: 'http://localhost:7320', default_model: null }]) {
    expect(() => service.setDefault('admin', body, 'request')).toThrow();
  }
  vi.mocked(identity.requireAdmin).mockRejectedValue(new Error('unauthorized'));
  await expect(service.setDefault('user', { public_origin: 'http://localhost:7320' }, 'request')).rejects.toThrow('unauthorized');
  expect(repository.update).not.toHaveBeenCalled();
});
