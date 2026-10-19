import { NetworkStreams } from './streams.js';
import { randomUUID } from 'node:crypto';
import type { EntityManager } from 'typeorm';
import type { LoginResponse } from '../contracts/identity.js';
import type { HostDeviceDetail } from '../contracts/host-devices.js';
import { IdentityError } from '../identity/errors.js';
import { IdentityService, type LoginScope } from '../identity/service.js';
import { fields, invalid } from '../validation.js';
import type { HostDevicesService } from './service.js';
import type { DeviceConnection } from './connection.js';

type Capability = 'ordinary' | 'desktop_view' | 'desktop_control';
type Access = {
  id: string;
  source: DeviceConnection;
  target: DeviceConnection;
  device: string;
  token: string;
  scope: LoginScope;
  capability: Capability;
  expires: number;
  resolve: (value: object) => void;
  reject: (error: Error) => void;
  sourceExpires: number;
  ready: boolean;
  issued?: LoginResponse;
  targetScope?: LoginScope;
  viewId?: string;
  desktop?: { prepared: boolean; offered: boolean; answered: boolean; sourceCandidates: number; targetCandidates: number };
};
/** 只保存活跃访问；事实来自现有身份与设备授权。对象归属隔离迟到消息和旧连接。 */
export class NetworkAccess {
  private readonly active = new Map<string, Access>();
  private readonly streams = new NetworkStreams((id, peer) => {
    const access = this.active.get(id);
    if (!access || !this.live(access)) return undefined;
    if (!access.ready || access.capability !== 'ordinary' || (peer !== access.source && peer !== access.target))
      throw new Error('invalid stream access');
    return access;
  });
  private readonly timer: ReturnType<typeof setInterval>;
  private readonly unsubscribe: () => void;
  constructor(
    private readonly identity: IdentityService,
    private readonly devices: HostDevicesService,
    private readonly target: (id: string) => DeviceConnection | undefined,
  ) {
    this.unsubscribe = identity.onSessionRevoked((scope) => {
      for (const access of this.active.values())
        if (access.scope === scope || access.targetScope === scope) this.end(access);
    });
    this.timer = setInterval(() => {
      for (const access of this.active.values())
        if (performance.now() >= Math.min(access.expires, access.sourceExpires)) this.end(access);
    }, 250);
    this.timer.unref();
  }

  close() {
    this.streams.close();
    clearInterval(this.timer);
    this.unsubscribe();
    for (const access of this.active.values()) this.end(access);
  }

  disconnected(connection: DeviceConnection) {
    for (const access of this.active.values())
      if (access.source === connection || access.target === connection) this.end(access);
  }

  changed(id: string, terminal: boolean, device?: HostDeviceDetail) {
    for (const access of this.active.values()) {
      if (access.device !== id) continue;
      const grant = device?.users.find((u) => u.user_id === access.scope.userId);
      if (
        terminal ||
        (device &&
          (device.status !== 'registered' ||
            !device.enabled ||
            !grant ||
            (access.capability === 'desktop_view' && !grant.desktop_view) ||
            (access.capability === 'desktop_control' && !grant.desktop_control)))
      )
        this.end(access);
    }
  }

  binary(connection: DeviceConnection, bytes: Buffer) {
    this.streams.binary(connection, bytes);
  }

  async dispatch(connection: DeviceConnection, value: unknown) {
    if (
      value &&
      typeof value === 'object' &&
      'type' in value &&
      typeof value.type === 'string' &&
      value.type.startsWith('stream_')
    ) {
      this.streams.control(connection, value as Record<string, unknown>);
      return;
    }
    const input = fields(value, ['type', 'request_id', 'action', 'payload']);
    if (input.type !== 'request' || typeof input.request_id !== 'string' || !/^[0-9a-f-]{36}$/.test(input.request_id))
      invalid();
    try {
      const result = await this.command(connection, input.action, input.payload);
      connection.send({ type: 'response', request_id: input.request_id, result }, (ok) => {
        if (!ok) this.disconnected(connection);
      });
    } catch (error) {
      connection.send({
        type: 'response',
        request_id: input.request_id,
        error: error instanceof IdentityError ? error.code : 'SERVICE_UNAVAILABLE',
      });
    }
  }

  private async command(connection: DeviceConnection, action: unknown, payload: unknown): Promise<object> {
    if (!connection.online) throw new IdentityError(409, 'DEVICE_ACCESS_ENDED');
    if (action === 'desktop_signal') return this.desktopSignal(connection, payload);
    if (action === 'directory') {
      const p = fields(payload, ['token', 'offset']);
      if (typeof p.token !== 'string' || !Number.isSafeInteger(p.offset) || (p.offset as number) < 0) invalid();
      return this.identity.write(async (manager) => {
        const user = await this.identity.requireUser(manager, p.token as string);
        return this.devices.directory(manager, user.id, p.offset as number);
      });
    }
    if (action === 'open') return this.open(connection, payload);
    const p = fields(payload, action === 'ready' || action === 'renew' ? ['access_id', 'target_token'] : ['access_id']);
    if (action === 'close') {
      if (typeof p.access_id !== 'string') invalid();
      // 与 open 的提交共用顺序：提前到达的 close 不能越过仍在鉴权的 open，留下无人续期的访问。
      await this.identity.write(
        async () => {
          const access = this.active.get(p.access_id as string);
          if (access && access.source !== connection && access.target !== connection)
            throw new IdentityError(403, 'DEVICE_ACCESS_DENIED');
          return access;
        },
        (access) => {
          if (access) this.end(access);
        },
      );
      return {};
    }
    const access = typeof p.access_id === 'string' ? this.active.get(p.access_id) : undefined;
    if (!access || !this.live(access)) throw new IdentityError(409, 'DEVICE_ACCESS_ENDED');
    if (action === 'renew_source') {
      if (access.source !== connection || !access.ready) throw new IdentityError(403, 'DEVICE_ACCESS_DENIED');
      return this.identity.write(
        async (manager) => {
          await this.authorize(manager, access);
          return { valid_for_ms: 45000 };
        },
        () => {
          if (this.live(access)) access.sourceExpires = performance.now() + 45000;
        },
      );
    }
    if (access.target !== connection) throw new IdentityError(403, 'DEVICE_ACCESS_DENIED');
    if (action === 'login') {
      if (access.ready || access.issued) throw new IdentityError(409, 'DEVICE_CHANGED');
      const login = await this.identity.write(
        async (manager) => {
          if (access.ready || access.issued) throw new IdentityError(409, 'DEVICE_CHANGED');
          const user = await this.authorize(manager, access);
          return this.identity.prepareLogin(manager, user, randomUUID(), 'device_user_login');
        },
        (login) => {
          if (this.live(access)) {
            this.identity.publishLogin(login);
            access.issued = login;
          }
        },
      );
      if (!this.live(access)) throw new IdentityError(409, 'DEVICE_ACCESS_ENDED');
      return login;
    }
    if (action === 'ready' || action === 'renew') {
      if (typeof p.target_token !== 'string') invalid();
      return this.identity.write(
        async (manager) => {
          await this.authorize(manager, access);
          const targetUser = await this.identity.requireUser(manager, p.target_token as string);
          if (targetUser.id !== access.scope.userId) throw new IdentityError(403, 'DEVICE_ACCESS_DENIED');
          if (!this.live(access) || (action === 'renew' && !access.ready))
            throw new IdentityError(409, 'DEVICE_ACCESS_ENDED');
          return { access_id: access.id, valid_for_ms: 45000 };
        },
        (result) => {
          if (!this.live(access)) return;
          access.targetScope = this.identity.scope(p.target_token as string);
          const unused = access.issued;
          access.issued = undefined;
          if (unused && unused.token !== p.target_token) this.identity.discardLogin(unused.token);
          access.ready = true;
          access.expires = performance.now() + 45000;
          access.resolve(result);
        },
      );
    }
    invalid();
  }

  private async open(source: DeviceConnection, payload: unknown) {
    const p = fields(payload, ['access_id', 'token', 'device_id', 'capability'], ['view_id']);
    if (
      typeof p.access_id !== 'string' ||
      !/^[0-9a-f-]{36}$/.test(p.access_id) ||
      typeof p.token !== 'string' ||
      typeof p.device_id !== 'string' ||
      !/^[0-9a-f-]{36}$/.test(p.device_id) ||
      !['ordinary', 'desktop_view', 'desktop_control'].includes(p.capability as string)
    )
      invalid();
    let resolve!: (value: object) => void, reject!: (error: Error) => void;
    const result = new Promise<object>((yes, no) => {
      resolve = yes;
      reject = no;
    });
    // 在异步事务提交前就观察拒绝，避免中途关闭导致未处理 rejection。
    void result.catch(() => {});
    await this.identity.write(
      async (manager) => {
        if (this.active.has(p.access_id as string)) throw new IdentityError(409, 'DEVICE_CHANGED');
        if (this.active.size >= 1024 || [...this.active.values()].filter((a) => a.source === source).length >= 64)
          throw new IdentityError(429, 'RATE_LIMITED');
        const user = await this.identity.requireUser(manager, p.token as string);
        await this.devices.allow(manager, user.id, p.device_id as string, p.capability as string);
        const view = typeof p.view_id === 'string' ? this.active.get(p.view_id) : undefined;
        if (p.capability === 'desktop_control') {
          if (!view || !this.live(view) || !view.ready || view.capability !== 'desktop_view' ||
              view.source !== source || view.token !== p.token || view.device !== p.device_id)
            throw new IdentityError(403, 'DEVICE_ACCESS_DENIED');
        } else if (p.view_id !== undefined) invalid();
        const target = this.target(p.device_id as string);
        if (source === target) throw new IdentityError(403, 'DEVICE_ACCESS_DENIED');
        if (!source.online || !target?.online) throw new IdentityError(409, 'DEVICE_ACCESS_ENDED');
        return {
          viewId: view?.id,
          id: p.access_id as string,
          source,
          target,
          device: p.device_id as string,
          token: p.token as string,
          scope: this.identity.scope(p.token as string),
          capability: p.capability as Capability,
          expires: performance.now() + 30000,
          sourceExpires: performance.now() + 45000,
          ready: false,
          resolve,
          reject,
        } satisfies Access;
      },
      (access) => {
        if (!source.online || !access.target.online || (access.viewId && !this.active.has(access.viewId))) {
          reject(new IdentityError(409, 'DEVICE_ACCESS_ENDED'));
          return;
        }
        this.active.set(access.id, access);
        access.target.send(
          {
            type: 'access_offer',
            access_id: access.id,
            user_id: access.scope.userId,
            capability: access.capability,
            source_connection_id: source.connectionId,
            ...(access.viewId ? { view_id: access.viewId } : {}),
            valid_for_ms: 30000,
          },
          (ok) => {
            if (!ok) this.end(access);
          },
        );
      },
    );
    return result;
  }

  private async authorize(manager: EntityManager, access: Access) {
    if (!this.live(access)) throw new IdentityError(409, 'DEVICE_ACCESS_ENDED');
    const user = await this.identity.requireUser(manager, access.token);
    await this.devices.allow(manager, user.id, access.device, access.capability);
    if (!this.live(access)) throw new IdentityError(409, 'DEVICE_ACCESS_ENDED');
    return user;
  }

  /** 只转发本次活跃许可的有界协商；来源由连接对象决定，不能由载荷伪造。 */
  private desktopSignal(connection: DeviceConnection, payload: unknown): object {
    const p = fields(payload, ['access_id', 'signal']);
    const access = typeof p.access_id === 'string' ? this.active.get(p.access_id) : undefined;
    if (!access || !this.live(access)) throw new IdentityError(409, 'DEVICE_ACCESS_ENDED');
    if (!access.ready || access.capability !== 'desktop_view' || (connection !== access.source && connection !== access.target))
      throw new IdentityError(403, 'DEVICE_ACCESS_DENIED');
    const source = connection === access.source;
    const signal = fields(p.signal, ['type', 'data']);
    const length = Buffer.byteLength(JSON.stringify(signal));
    const state = access.desktop ??= { prepared: false, offered: false, answered: false, sourceCandidates: 0, targetCandidates: 0 };
    let valid = false;
    switch (signal.type) {
      case 'prepare': valid = source && !state.prepared && length <= 1024; if (valid) state.prepared = true; break;
      case 'offer': valid = !source && state.prepared && !state.offered && length <= 8192; if (valid) state.offered = true; break;
      case 'answer': valid = source && state.offered && !state.answered && length <= 8192; if (valid) state.answered = true; break;
      case 'candidate': {
        const count = source ? ++state.sourceCandidates : ++state.targetCandidates;
        valid = state.prepared && count <= 64 && length <= 1024; break;
      }
      case 'error': valid = !source && length <= 512; break;
    }
    if (!valid) { this.end(access); throw new IdentityError(403, 'DEVICE_ACCESS_DENIED'); }
    const recipient = source ? access.target : access.source;
    recipient.send({ type: 'desktop_signal', access_id: access.id, signal }, (ok) => { if (!ok) this.end(access); });
    return {};
  }

  private live(access: Access) {
    return (
      this.active.get(access.id) === access &&
      access.source.online &&
      access.target.online &&
      this.target(access.device) === access.target &&
      performance.now() < Math.min(access.expires, access.sourceExpires)
    );
  }

  private end(access: Access) {
    if (!this.active.delete(access.id)) return;
    for (const child of this.active.values()) if (child.viewId === access.id) this.end(child);
    this.streams.revoke(access.id);
    const issued = access.issued;
    access.issued = undefined;
    if (issued) this.identity.discardLogin(issued.token);
    access.reject(new IdentityError(409, 'DEVICE_ACCESS_ENDED'));
    for (const peer of new Set([access.source, access.target]))
      peer.send({ type: 'access_revoked', access_id: access.id });
  }
}
