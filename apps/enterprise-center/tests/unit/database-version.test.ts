import { expect, it, vi } from 'vitest';
import type { PoolClient } from 'pg';
import { checkServer } from '../../dist/database/admission.js';

it.each(['160014', '160015'])('允许已选择主版本及同线补丁：%s', async version => {
  const client = { query: vi.fn().mockResolvedValue({ rows: [{ version }] }) } as unknown as PoolClient;
  await expect(checkServer(client)).resolves.toBeUndefined();
});

it.each(['140018', '160013', '170000', '180006', 'invalid'])('拒绝未支持的版本：%s', async version => {
  const client = { query: vi.fn().mockResolvedValue({ rows: [{ version }] }) } as unknown as PoolClient;
  await expect(checkServer(client)).rejects.toMatchObject({ code: 'DATABASE_VERSION_UNSUPPORTED' });
});
