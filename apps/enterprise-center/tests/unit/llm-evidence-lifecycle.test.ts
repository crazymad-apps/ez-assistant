import { randomUUID } from 'node:crypto';
import { expect, it, vi } from 'vitest';
import { createDataSource } from '../../dist/database/source.js';
import { CallEntity, RecordingEntity } from '../../dist/llm/entity.js';
import { CallRecording, CallRecords } from '../../dist/llm/records.js';
import { SnapshotFiles, SnapshotMemory, noSnapshot } from '../../dist/llm/snapshots.js';
import { ResponseObserver } from '../../dist/llm/response-observer.js';
import { CallManagement } from '../../dist/llm/management.js';
import { IdentityService } from '../../dist/identity/service.js';
import { ModelsService } from '../../dist/models/service.js';
import { ModelTemplates } from '../../dist/models/templates.js';
import { ModelProxy } from '../../dist/llm/proxy.js';

it('开始时冻结模式；明确正常才释放，文件失败独立，所有采集预算收尾释放', async () => {
  const source = createDataSource({ url: 'postgres://unused@127.0.0.1/unused' });
  const repository = source.getRepository(CallEntity);
  const files = new SnapshotFiles();
  const records = new CallRecords(source, files);
  const settings = {
    singleton: true,
    mode: 'anomalies' as const,
    content_retention_days: 7,
    index_retention_days: 90,
    usage_available_from: new Date(),
  };
  vi.spyOn(source.getRepository(RecordingEntity), 'findOneByOrFail').mockResolvedValue(settings);
  vi.spyOn(repository, 'insert').mockResolvedValue({ identifiers: [], generatedMaps: [], raw: [] });
  const update = vi.spyOn(repository, 'update').mockResolvedValue({ affected: 1, generatedMaps: [], raw: [] });
  vi.spyOn(repository, 'create').mockReturnValue(
    Object.assign(new CallEntity(), {
      id: randomUUID(),
      started_at: new Date(),
      protocol: 'open_ai_responses',
      kind: 'proxy',
    }),
  );
  const publish = vi.spyOn(files, 'publish').mockRejectedValue(new Error('disk failure'));
  try {
    const first = await records.begin({
      user_id: 1,
      username: 'u',
      model_id: 'm',
      provider_instance_id: randomUUID(),
      protocol: 'open_ai_responses',
      kind: 'proxy',
    });
    Object.assign(settings, { mode: 'all' });
    first.request!.append(Buffer.from('request'));
    first.observation = new ResponseObserver(first.row);
    first.headers({ 'content-type': 'application/json' });
    first.chunk(Buffer.from('{"id":"r","status":"completed","output":[]}'));
    await Promise.all([first.settle('forwarded', 200, null), first.settle('interrupted', null, 'cancelled')]);
    expect(publish).not.toHaveBeenCalled();
    expect(update.mock.calls.filter(([, value]) => 'response_snapshot_state' in value).at(-1)?.[1]).toMatchObject({
      response_snapshot_state: 'disabled',
      response_snapshot_reason: 'normal_not_retained',
    });
    expect(records.active.size).toBe(0);
    const memory = new SnapshotMemory();
    const bad = new CallRecording(
      Object.assign(new CallEntity(), {
        id: randomUUID(),
        started_at: new Date(),
        protocol: 'open_ai_responses',
        kind: 'proxy',
      }),
      records,
      memory,
      'all',
    );
    bad.request!.append(Buffer.from('request'));
    bad.failureFrame(Buffer.from([0xff]));
    bad.failureFrame(Buffer.from('second must not replace first'));
    bad.observation = new ResponseObserver(bad.row);
    bad.headers({ 'content-type': 'application/json' });
    await bad.settle('interrupted', 200, 'upstream_connection');
    expect(
      update.mock.calls.some(
        ([, value]) =>
          'upstream_failure_snapshot' in value &&
          typeof value.upstream_failure_snapshot === 'object' &&
          value.upstream_failure_snapshot?.state === 'failed',
      ),
    ).toBe(true);
    expect(update.mock.calls.some(([, value]) => value.response_snapshot_state === 'not_received')).toBe(true);
    // request/失败帧写入均失败也释放持有页；late callback 不得重新占用预算。
    bad.failureFrame(Buffer.alloc(1024));
    expect(memory.reserve(64 * 1024 * 1024)).toBe(64 * 1024 * 1024);
  } finally {
    vi.restoreAllMocks();
  }
});

it('实际收到字节但采集预算耗尽时标采集失败，不误标未收到', async () => {
  const source = createDataSource({ url: 'postgres://unused@127.0.0.1/unused' });
  const update = vi
    .spyOn(source.getRepository(CallEntity), 'update')
    .mockResolvedValue({ affected: 1, generatedMaps: [], raw: [] });
  const files = new SnapshotFiles();
  vi.spyOn(files, 'publish').mockResolvedValue(noSnapshot('complete'));
  const memory = new SnapshotMemory();
  memory.reserve(64 * 1024 * 1024);
  const record = new CallRecording(
    Object.assign(new CallEntity(), {
      id: randomUUID(),
      started_at: new Date(),
      kind: 'proxy',
      protocol: 'open_ai_responses',
    }),
    new CallRecords(source, files),
    memory,
    'all',
  );
  record.headers({ 'content-type': 'application/json' });
  record.chunk(Buffer.from('received'));
  await record.settle('interrupted', 200, 'upstream_connection');
  expect(
    update.mock.calls.some(
      ([, value]) => value.response_snapshot_state === 'failed' && value.response_snapshot_reason === 'memory_limit',
    ),
  ).toBe(true);
  vi.restoreAllMocks();
});

it('正文两个读取槽，第三次立即繁忙；审计或身份复核失败不返回正文，槽最终释放', async () => {
  const source = createDataSource({ url: 'postgres://unused@127.0.0.1/unused' });
  const identities = new IdentityService();
  const models = new ModelsService(identities, undefined, new ModelTemplates());
  const records = new CallRecords(source, new SnapshotFiles());
  const proxy = new ModelProxy(identities, models, records, {});
  const management = new CallManagement(identities, source, proxy, models);
  const row = Object.assign(new CallEntity(), {
    id: randomUUID(),
    started_at: new Date(),
    response_snapshot_state: 'complete',
    response_bytes: '6',
    response_sha256: 'digest',
    response_snapshot_reason: null,
  });
  vi.spyOn(source.manager.getRepository(CallEntity), 'findOneBy').mockResolvedValue(row);
  vi.spyOn(identities, 'requireAdmin').mockResolvedValue({
    id: 1,
    username: 'u',
    display_name: 'u',
    role: 'admin',
    enabled: true,
    is_super_admin: false,
  });
  vi.spyOn(identities, 'write').mockImplementation((work) => work(source.manager));
  const audit = vi.spyOn(identities, 'audit').mockResolvedValue();
  let resume!: () => void;
  const gate = new Promise<void>((done) => {
    resume = done;
  });
  const read = vi.spyOn(records.files, 'read').mockImplementation(async () => {
    await gate;
    return { body: Buffer.from('secret'), result: { ...noSnapshot('complete'), bytes: 6, sha256: 'digest' } };
  });
  try {
    const first = management.snapshot('token', row.id, 'response', randomUUID());
    const second = management.snapshot('token', row.id, 'response', randomUUID());
    await expect(management.snapshot('token', row.id, 'response', randomUUID())).rejects.toMatchObject({
      code: 'SNAPSHOT_READ_BUSY',
    });
    audit.mockRejectedValue(new Error('audit failed'));
    resume();
    await expect(first).rejects.toThrow('audit failed');
    await expect(second).rejects.toThrow('audit failed');
    audit.mockResolvedValue();
    expect(await management.snapshot('token', row.id, 'response', randomUUID())).toMatchObject({
      text: 'secret',
      body_base64: null,
    });
    expect(read).toHaveBeenCalledTimes(3);
  } finally {
    resume();
    await proxy.close();
    vi.restoreAllMocks();
  }
});
