import { expect, it, vi } from 'vitest';
import type { DataSource, EntityManager } from 'typeorm';
import { createDataSource } from '../../dist/database/source.js';
import { IdentityService } from '../../dist/identity/service.js';

it('业务 DataSource 禁止隐式升级/同步/扩展安装，使用有界连接且不输出原始诊断', () => {
  const source = createDataSource({ url: 'postgresql://test:test@127.0.0.1:55432/test' });
  expect(source.isInitialized).toBe(false);
  expect(source.options).toMatchObject({
    type: 'postgres',
    synchronize: false,
    migrationsRun: false,
    migrations: [],
    dropSchema: false,
    installExtensions: false,
    logging: false,
    poolSize: 8,
    connectTimeoutMS: 3000,
    extra: { statement_timeout: 5000, lock_timeout: 2000, idle_in_transaction_session_timeout: 10000 },
  });
  expect(source.options.entities).toHaveLength(8);
});

function fixture() {
  const events: string[] = [],
    manager = {} as EntityManager;
  const runner = {
    manager,
    isTransactionActive: false,
    connect: vi.fn(async () => {
      events.push('connect');
    }),
    startTransaction: vi.fn(async () => {
      runner.isTransactionActive = true;
      events.push('begin');
    }),
    commitTransaction: vi.fn(async () => {
      events.push('commit');
      runner.isTransactionActive = false;
    }),
    rollbackTransaction: vi.fn(async () => {
      events.push('rollback');
      runner.isTransactionActive = false;
    }),
    release: vi.fn(async () => {
      events.push('release');
    }),
  };
  const source = {
    isInitialized: true,
    createQueryRunner: vi.fn(() => runner),
    destroy: vi.fn(async () => {
      source.isInitialized = false;
      events.push('destroy');
    }),
  };
  const identity = new IdentityService(source as unknown as DataSource);
  const clear = vi.spyOn(identity, 'onModuleDestroy');
  return { events, manager, runner, source, identity, clear };
}

it('串行段传入事务 Manager，COMMIT 成功后才变更内存；失败后队列仍可继续', async () => {
  const f = fixture();
  const failed = f.identity.write(async (manager) => {
    expect(manager).toBe(f.manager);
    f.events.push('work');
    throw new Error('work');
  });
  const next = f.identity.write(
    async () => {
      f.events.push('next');
      return 1;
    },
    (result) => {
      expect(result).toBe(1);
      f.events.push('memory');
    },
  );
  await expect(failed).rejects.toThrow('work');
  await expect(next).resolves.toBe(1);
  expect(f.events).toEqual([
    'connect',
    'begin',
    'work',
    'rollback',
    'release',
    'connect',
    'begin',
    'next',
    'commit',
    'memory',
    'release',
  ]);
  expect(f.clear).not.toHaveBeenCalled();
});

it('COMMIT 结果不明清空凭据、不重试、不调用成功回调', async () => {
  const f = fixture(),
    committed = vi.fn();
  f.runner.commitTransaction.mockRejectedValueOnce(new Error('commit lost'));
  await expect(f.identity.write(async () => 1, committed)).rejects.toThrow('commit lost');
  expect(f.clear).toHaveBeenCalledOnce();
  expect(committed).not.toHaveBeenCalled();
  expect(f.runner.commitTransaction).toHaveBeenCalledOnce();
  expect(f.runner.rollbackTransaction).toHaveBeenCalledOnce();
  expect(f.runner.release).toHaveBeenCalledOnce();
});

it('回滚也失败时关闭业务池并撤销凭据，后续业务拒绝而不是复用未知事务', async () => {
  const f = fixture();
  f.runner.rollbackTransaction.mockRejectedValueOnce(new Error('rollback lost'));
  await expect(
    f.identity.write(async () => {
      throw new Error('original');
    }),
  ).rejects.toThrow('original');
  expect(f.clear).toHaveBeenCalledOnce();
  expect(f.source.destroy).toHaveBeenCalledOnce();
  await expect(f.identity.write(async () => 1)).rejects.toMatchObject({ code: 'SERVICE_UNAVAILABLE' });
  expect(f.source.createQueryRunner).toHaveBeenCalledOnce();
});
