import { randomUUID, randomBytes } from 'node:crypto';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdir, readFile, stat, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { expect } from 'vitest';
import type { HostDeviceDetail, HostDeviceState } from '../../dist/contracts/host-devices.js';

type Registration = {
  registration_id: string | null;
  installation_id: string | null;
  intent: string | null;
  error: { message: string } | null;
  device: { id: string; status: string; online: boolean } | null;
};
type Options = {
  root: string;
  origin: string;
  hostPort: number;
  centerId: string;
  adminToken: string;
  restartCenter: () => Promise<string>;
  stopCenter: () => Promise<void>;
};

/** 只从测试主场景调用；不创建额外产品调试 API，不读取用户运行目录。 */
export async function exerciseHost(options: Options) {
  const binary = resolve('../../target/debug/ez-assistant-runtime');
  const home = join(options.root, 'host');
  await mkdir(home, { mode: 0o700 });
  await writeFile(
    join(home, 'host.toml'),
    `version = "0.27.0"\nmode = "enterprise"\n[enterprise]\ncenter_url = "${options.origin}"\n[host_access]\nport = ${options.hostPort}\nremote_enabled = false\nscheme = "http"\nserver_names = []\n`,
    { mode: 0o600 },
  );
  let child: ChildProcess | undefined;
  let bootstrap = '',
    user = '';
  const host = `http://127.0.0.1:${options.hostPort}`;
  const version = { 'x-ez-client-version': '0.32.0', 'x-ez-min-compatible-version': '0.32.0' };
  async function start() {
    child = spawn(binary, ['serve', '--runtime-home', home], { stdio: 'ignore' });
    for (let i = 0; i < 150; i++) {
      if (child.exitCode !== null) throw new Error(`isolated Host exited: ${child.exitCode}`);
      try {
        const discovery = JSON.parse(await readFile(join(home, 'run/runtime.json'), 'utf8')) as {
          access_token: string;
          pid: number;
        };
        if (discovery.pid === child.pid) {
          bootstrap = discovery.access_token;
          return;
        }
      } catch {
        /* 等待新实例发布，不复用上次发现文件。 */
      }
      await delay(100);
    }
    throw new Error('isolated Host startup timeout');
  }
  async function stop() {
    if (!child || child.exitCode !== null) return;
    const stopped = new Promise<void>((resolve, reject) => {
      child!.once('exit', () => resolve());
      child!.once('error', reject);
    });
    child.kill('SIGINT');
    await Promise.race([
      stopped,
      delay(10000).then(() => {
        throw new Error('isolated Host stop timeout');
      }),
    ]);
  }
  async function hostRequest(path: string, body: unknown, token = bootstrap, proof = true) {
    return fetch(`${host}${path}`, {
      method: 'POST',
      headers: {
        ...version,
        'content-type': 'application/json',
        ...(token ? { authorization: `Bearer ${token}` } : {}),
        ...(proof ? { 'x-ez-host-bootstrap': bootstrap } : {}),
      },
      body: JSON.stringify(body),
    });
  }
  async function command(type: string, name?: string, token = bootstrap) {
    const response = await hostRequest(
      '/host-network/registration',
      { type, ...(name ? { payload: { name } } : {}) },
      token,
    );
    expect(response.status).toBe(200);
    return (await response.json()) as Registration;
  }
  async function login() {
    const response = await hostRequest(
      '/auth/login',
      { method: 'enterprise', username: 'device-b', password: 'test1234', native: true },
      '',
    );
    expect(response.status).toBe(200);
    user = ((await response.json()) as { token: string }).token;
  }
  async function centerDevice(id: string) {
    const response = await fetch(`${options.origin}/api/admin/host-devices/${id}`, {
      headers: { authorization: `Bearer ${options.adminToken}` },
    });
    expect(response.status).toBe(200);
    return (await response.json()) as HostDeviceDetail;
  }
  async function approve(id: string) {
    const current = await centerDevice(id);
    const expected: HostDeviceState = {
      status: current.status,
      name: current.name,
      enabled: current.enabled,
      users: current.users,
    };
    const response = await fetch(`${options.origin}/api/admin/host-devices/${id}`, {
      method: 'PATCH',
      headers: { authorization: `Bearer ${options.adminToken}`, 'content-type': 'application/json' },
      body: JSON.stringify({ expected, desired: { ...expected, status: 'registered' } }),
    });
    expect(response.status).toBe(200);
  }
  try {
    await start();
    expect(await command('get_status')).toMatchObject({ device: null, registration_id: null, error: null });
    expect(
      (await hostRequest('/host-network/registration', { type: 'apply', payload: { name: '服务器' } })).status,
    ).toBe(401);
    await login();
    expect(
      (await hostRequest('/host-network/registration', { type: 'apply', payload: { name: '服务器' } }, user, false))
        .status,
    ).toBe(403);
    // 测试中心在已提交申请后丢失响应；正式 Host 必须保留原 ID 供下次查询恢复。
    const unknown = await command('apply', '隔离 Host', user);
    expect(unknown.error).not.toBeNull();
    const pending = await command('get_status');
    expect(pending).toMatchObject({
      error: null,
      registration_id: unknown.registration_id,
      device: { status: 'pending' },
    });
    expect((await command('apply', '重复申请名称', user)).registration_id).toBe(pending.registration_id);
    const access = await hostRequest('/commands', {
      request_id: randomUUID(),
      command: { scope: 'host_access', payload: { type: 'get_status' } },
    });
    const accessState = ((await access.json()) as { result: { payload: { revision: string; configuration: object } } })
      .result.payload;
    const rebind = await hostRequest('/commands', {
      request_id: randomUUID(),
      command: {
        scope: 'host_access',
        payload: {
          type: 'configure',
          payload: {
            expected_revision: accessState.revision,
            configuration: accessState.configuration,
            clear_center_binding: true,
          },
        },
      },
    });
    expect(rebind.status).not.toBe(200);
    const identityPath = join(home, 'network/identity.json');
    expect((await stat(identityPath)).mode & 0o777).toBe(0o600);
    const privateIdentity = JSON.parse(await readFile(identityPath, 'utf8')) as { current: { secret: string } };
    expect(JSON.stringify(pending)).not.toContain(privateIdentity.current.secret);
    // 用户退出不撤回申请；设备离线时管理员仍可以确认。
    expect((await hostRequest('/auth/logout', {}, user)).status).toBe(200);
    await stop();
    await approve(pending.registration_id!);
    await start();
    const registered = await command('get_status');
    expect(registered).toMatchObject({
      registration_id: pending.registration_id,
      installation_id: pending.installation_id,
      error: null,
      device: { status: 'registered' },
    });
    expect((await centerDevice(registered.registration_id!)).users).toEqual([]);
    const conflict = await command('withdraw');
    expect(conflict.error).not.toBeNull();
    expect((await centerDevice(registered.registration_id!)).status).toBe('registered');
    // 退出先保存本机意图；中心停机和 Host 重启不能把退出变回发布。
    await options.stopCenter();
    const deferred = await command('exit');
    expect(deferred.intent).toBe('exit');
    expect(deferred.error).not.toBeNull();
    await stop();
    await start();
    expect((await command('get_status')).intent).toBe('exit');
    options.adminToken = await options.restartCenter();
    const closed = await command('get_status');
    expect(closed).toMatchObject({ intent: 'closed', error: null, device: { status: 'revoked' } });
    await login();
    const renewed = await command('apply', '重登 Host', user);
    expect(renewed).toMatchObject({
      installation_id: pending.installation_id,
      error: null,
      device: { status: 'pending' },
    });
    expect(renewed.registration_id).not.toBe(pending.registration_id);
    expect((await command('withdraw')).device?.status).toBe('withdrawn');
    // 模拟原子保存之后、首个申请包发送之前进程停止；null 不能被当成退出已确认。
    await stop();
    const saved = JSON.parse(await readFile(identityPath, 'utf8')) as {
      installation_id: string;
      current: Record<string, unknown>;
    };
    saved.current = { ...saved.current, id: randomUUID(), secret: randomBytes(32).toString('hex'), intent: 'publish' };
    await writeFile(identityPath, JSON.stringify(saved), { mode: 0o600 });
    await start();
    const unsent = await command('exit');
    expect(unsent.intent).toBe('exit');
    expect(unsent.error).not.toBeNull();
    await login();
    const recovered = await command('get_status', undefined, user);
    expect(recovered).toMatchObject({
      intent: 'closed',
      registration_id: saved.current.id,
      error: null,
      device: { status: 'revoked' },
    });
  } finally {
    await stop();
  }
}
