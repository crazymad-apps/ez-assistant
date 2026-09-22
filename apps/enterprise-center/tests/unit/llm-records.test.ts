import { mkdtemp, mkdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { expect, it, vi } from 'vitest';
import type { SelectQueryBuilder } from 'typeorm';
import { createDataSource } from '../../dist/database/source.js';
import { CallRecords } from '../../dist/llm/records.js';
import { CallEntity, RecordingEntity } from '../../dist/llm/entity.js';
import { SnapshotFiles, Capture, SnapshotMemory } from '../../dist/llm/snapshots.js';
import { ModelProxy } from '../../dist/llm/proxy.js';
import { ModelsService } from '../../dist/models/service.js';
import { ModelTemplates } from '../../dist/models/templates.js';
import { IdentityService } from '../../dist/identity/service.js';

it('最小索引无法建立时不准入或发送上游', async () => {
  const identity = new IdentityService();
  const models = new ModelsService(identity, undefined, new ModelTemplates());
  const admit = vi.spyOn(models, 'admit');
  const proxy = new ModelProxy(identity, models, new CallRecords(undefined, new SnapshotFiles()), {});
  await expect(
    proxy.execute({
      key: 'test',
      user: { id: 1, username: 'test', display_name: 'test', role: 'admin', is_super_admin: false, enabled: true },
      selection: { provider_instance_id: randomUUID(), model_id: 'model' },
      protocol: 'open_ai_responses',
      body: Buffer.from('{}'),
      signal: new AbortController().signal,
      kind: 'proxy',
      sink: { headers() {}, async chunk() {} },
    }),
  ).rejects.toMatchObject({ code: 'llm_record_unavailable' });
  expect(admit).not.toHaveBeenCalled();
  await proxy.close();
});

it('文件已发布而索引未结算时恢复为 unknown/partial；清理先文件后索引且异常立即停止', async () => {
  const parent = resolve('../../.runtime-test/c04-m1-record-recovery');
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, 'case-'));
  const files = new SnapshotFiles(root);
  await files.validate();
  const row = Object.assign(new CallEntity(), {
    id: randomUUID(),
    started_at: new Date('2020-01-01T00:00:00Z'),
    ended_at: null,
    outcome: 'in_progress',
    request_snapshot_state: 'pending',
    response_snapshot_state: 'disabled',
  });
  const capture = new Capture(new SnapshotMemory());
  capture.append(Buffer.from('saved before crash'));
  await files.publish(row.id, row.started_at, 'request', capture);
  capture.release();
  // 仅替换 ORM 方法模拟故障；此 DataSource 从不 initialize/connect，不访问任何数据库。
  const source = createDataSource({ url: 'postgresql://unused@127.0.0.1/unused' });
  const repository = source.getRepository(CallEntity);
  vi.spyOn(source.getRepository(RecordingEntity), 'findOneByOrFail').mockResolvedValue({
    singleton: true,
    enabled: true,
    content_retention_days: 7,
    index_retention_days: 90,
  });
  const getMany = vi.fn().mockResolvedValueOnce([row]).mockResolvedValueOnce([]);
  const query = {
    where: vi.fn().mockReturnThis(),
    andWhere: vi.fn().mockReturnThis(),
    orderBy: vi.fn().mockReturnThis(),
    take: vi.fn().mockReturnThis(),
    getMany,
  };
  vi.spyOn(repository, 'createQueryBuilder').mockReturnValue(query as unknown as SelectQueryBuilder<CallEntity>);
  const update = vi.spyOn(repository, 'update').mockImplementation(async (_id, patch) => {
    Object.assign(row, patch);
    return { affected: 1, raw: [], generatedMaps: [] };
  });
  const records = new CallRecords(source, files);
  try {
    await records.initialize();
    expect(row.outcome).toBe('unknown');
    expect(row.request_snapshot_state).toBe('partial');
    expect(row.request_snapshot_reason).toBe('recovered_unknown');
    expect(row.request_sha256).toBeTruthy();
    const clean = vi.spyOn(files, 'clean');
    const deleted = vi.spyOn(repository, 'delete').mockImplementation(async () => {
      expect(await files.read(row.id, row.started_at, 'request')).toBeNull();
      return { affected: 1, raw: [] };
    });
    getMany.mockResolvedValueOnce([row]);
    await records.cleanup();
    expect(deleted).toHaveBeenCalledTimes(1);
    getMany.mockResolvedValueOnce([row]);
    clean.mockRejectedValueOnce(new Error('disk failure'));
    const previousUpdates = update.mock.calls.length;
    await expect(records.cleanup()).rejects.toThrow('disk failure');
    expect(deleted).toHaveBeenCalledTimes(1);
    expect(update).toHaveBeenCalledTimes(previousUpdates);
  } finally {
    await records.close();
    vi.restoreAllMocks();
  }
});

it('服务关闭取消正在转发的上游，并等待唯一一次索引结算后返回', async () => {
  const { createServer } = await import('node:http');
  const { CallRecording } = await import('../../dist/llm/records.js');
  const { emptyParameters } = await import('../../dist/models/parameters.js');
  const server = createServer((request, response) => {
    request.resume();
    request.once('end', () => {
      response.writeHead(200);
      response.write('data: pending\n\n');
    });
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('missing port');
  const source = createDataSource({ url: 'postgresql://unused@127.0.0.1/unused' });
  const update = vi
    .spyOn(source.getRepository(CallEntity), 'update')
    .mockResolvedValue({ affected: 1, raw: [], generatedMaps: [] });
  const identities = new IdentityService();
  const models = new ModelsService(identities, undefined, new ModelTemplates());
  const selection = { provider_instance_id: randomUUID(), model_id: 'm' };
  vi.spyOn(models, 'admit').mockResolvedValue({
    provider: {
      ...selection,
      api_key: '',
      connection: {
        display_name: 'test',
        provider_type: 'openai',
        endpoint: `http://127.0.0.1:${address.port}/v1`,
        protocol_preference: 'responses',
        models_path: '',
        discovery_format: 'openai',
      },
      model_catalog: null,
      catalog_refreshed_at: null,
      catalog_connection_changed: false,
    },
    configuration: {
      ...selection,
      origin: 'manual',
      source: 'fixed',
      parameters: emptyParameters(),
      field_sources: {},
      updated_at_ms: null,
      template_document: null,
      template_checked_on: null,
      requires_configuration: false,
    },
  });
  const records = new CallRecords(source, new SnapshotFiles());
  const row = Object.assign(new CallEntity(), { id: randomUUID(), started_at: new Date(), provider_name: null });
  vi.spyOn(records, 'begin').mockResolvedValue(new CallRecording(row, records, new SnapshotMemory(), false));
  const proxy = new ModelProxy(identities, models, records, {});
  let started!: () => void;
  const arriving = new Promise<void>((resolve) => {
    started = resolve;
  });
  const pending = proxy.execute({
    key: 'test',
    user: { id: 1, username: 'u', display_name: 'u', role: 'admin', is_super_admin: false, enabled: true },
    selection,
    protocol: 'open_ai_responses',
    body: Buffer.from('{}'),
    signal: new AbortController().signal,
    kind: 'proxy',
    sink: { headers: started, async chunk() {} },
  });
  const failed = expect(pending).rejects.toMatchObject({ reason: 'cancelled' });
  try {
    await arriving;
    await proxy.close();
    await failed;
    expect(update).toHaveBeenCalledTimes(1);
    expect(update.mock.calls[0]?.[1]).toMatchObject({
      outcome: 'interrupted',
      reason: 'cancelled',
      provider_name: 'test',
    });
  } finally {
    await proxy.close();
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
    vi.restoreAllMocks();
  }
});
