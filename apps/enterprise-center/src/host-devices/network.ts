import type { IncomingMessage } from 'node:http';
import type { Duplex } from 'node:stream';
import type { FastifyInstance } from 'fastify';
import { WebSocketServer } from 'ws';
import { IdentityError } from '../identity/errors.js';
import type { HostDevicesService } from './service.js';
import type { HostDeviceDetail } from '../contracts/host-devices.js';
import { IdentityService } from '../identity/service.js';
import { NetworkAccess } from './access.js';
import { DeviceConnection } from './connection.js';

/** 唯一设备连接表。登记持久化归 service；在线只由当前连接对象产生。 */
export class HostNetwork {
  private readonly server = new WebSocketServer({
    noServer: true,
    maxPayload: 32 * 1024 + 30,
    perMessageDeflate: false,
    autoPong: false,
  });
  private readonly connections = new Map<string, DeviceConnection>();
  private readonly callers = new Set<DeviceConnection>();
  private readonly access: NetworkAccess;
  private readonly pending = new Set<Duplex>();
  private closing = false;
  private authenticating = 0;

  constructor(
    private readonly devices: HostDevicesService,
    identity: IdentityService,
  ) {
    this.access = new NetworkAccess(identity, devices, (id) => this.connections.get(id));
    devices.attachConnection(this);
  }

  install(app: FastifyInstance) {
    app.server.on('upgrade', (request, socket, head) => {
      if (request.url !== '/api/host-network/connect') return socket.destroy();
      if (this.closing || this.authenticating >= 64) return this.reject(socket, 503);
      this.pending.add(socket);
      this.authenticating++;
      const timeout = setTimeout(() => socket.destroy(), 5000);
      socket.once('close', () => {
        clearTimeout(timeout);
        this.pending.delete(socket);
      });
      // 升级请求不经过 Nest Guard，凭据只接受请求头，绝不接受 URL Token。
      void this.upgrade(request, socket, head).finally(() => {
        clearTimeout(timeout);
        this.pending.delete(socket);
        this.authenticating--;
      });
    });
    app.addHook('preClose', async () => this.onModuleDestroy());
  }

  online(id: string) {
    return this.connections.get(id)?.online ?? false;
  }

  onlineIds() {
    return [...this.connections].filter(([, connection]) => connection.online).map(([id]) => id);
  }

  changed(id: string, terminal: boolean, device?: HostDeviceDetail) {
    this.access.changed(id, terminal, device);
    const current = this.connections.get(id);
    if (!current) return;
    if (terminal) current.stop();
    else current.changed();
  }

  onModuleDestroy() {
    this.closing = true;
    this.access.close();
    for (const connection of this.callers) connection.stop();
    this.callers.clear();
    for (const socket of this.pending) socket.destroy();
    for (const connection of this.connections.values()) connection.stop();
    this.pending.clear();
    this.connections.clear();
  }

  private async upgrade(request: IncomingMessage, socket: Duplex, head: Buffer) {
    try {
      const id = request.headers['x-ez-device-id'];
      const bearer = request.headers.authorization;
      const caller = request.headers['x-ez-network-mode'] === 'caller';
      if ((!caller && typeof id !== 'string') || (caller && id !== undefined) || !bearer?.startsWith('Bearer '))
        return this.reject(socket, 401);
      if (caller && this.callers.size >= 256) return this.reject(socket, 503);
      const accepted = (center: string) => {
        // 事务提交后同步接纳，与注销通知有确定顺序；迟到结果不得恢复已关闭的连接。
        if (socket.destroyed || this.closing) return;
        if (caller && this.callers.size >= 256) return this.reject(socket, 503);
        if (typeof id === 'string' && this.connections.has(id)) return this.reject(socket, 409);
        try {
          this.server.handleUpgrade(request, socket, head, (ws) => {
            const connection = new DeviceConnection(
              ws,
              center,
              typeof id === 'string' ? id : null,
              () => {
                this.access.disconnected(connection);
                this.callers.delete(connection);
                if (typeof id === 'string' && this.connections.get(id) === connection) this.connections.delete(id);
              },
              (value) => this.access.dispatch(connection, value),
              (bytes) => this.access.binary(connection, bytes),
            );
            if (typeof id === 'string') this.connections.set(id, connection);
            else this.callers.add(connection);
            connection.start();
          });
        } catch {
          socket.destroy();
        }
      };
      if (caller) await this.devices.admitCaller(bearer.slice(7), accepted);
      else await this.devices.admit(id as string, bearer.slice(7), accepted);
    } catch (error) {
      this.reject(socket, error instanceof IdentityError ? error.status : 503);
    }
  }

  private reject(socket: Duplex, status: number) {
    if (socket.destroyed) return;
    const deadline = setTimeout(() => socket.destroy(), 5000);
    socket.once('close', () => clearTimeout(deadline));
    socket.end(`HTTP/1.1 ${status} Rejected\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`, () => socket.destroy());
  }
}
