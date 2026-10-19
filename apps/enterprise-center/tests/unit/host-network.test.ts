import { EventEmitter } from 'node:events';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { WebSocket } from 'ws';
import { DeviceConnection } from '../../src/host-devices/connection.js';

class Socket extends EventEmitter {
  bufferedAmount = 0;
  terminated = false;
  blocked = false;
  probes: Buffer[] = [];
  texts: string[] = [];
  callbacks: (() => void)[] = [];
  send(text: string, done: () => void) {
    this.texts.push(text);
    if (!this.blocked) done();
    else this.callbacks.push(done);
  }
  ping(data: Buffer, _mask: boolean, done: () => void) {
    this.probes.push(data);
    if (!this.blocked) done();
  }
  terminate() {
    this.terminated = true;
    this.emit('close');
  }
}
function fixture(ready = true) {
  const socket = new Socket();
  const closed = vi.fn();
  const connection = new DeviceConnection(socket as unknown as WebSocket, 'center', 'device', closed);
  connection.start();
  if (ready)
    socket.emit(
      'message',
      Buffer.from(
        JSON.stringify({ type: 'ready', protocol_version: 2, center_id: 'center', registration_id: 'device' }),
      ),
      false,
    );
  return { socket, connection, closed };
}
beforeEach(() => vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'performance'] }));
afterEach(() => vi.useRealTimers());
describe('设备控制连接期限和有界写入', () => {
  it('握手未确认不能在线，超时关闭一次', () => {
    const { socket, connection, closed } = fixture(false);
    expect(connection.online).toBe(false);
    vi.advanceTimersByTime(5000);
    expect(socket.terminated).toBe(true);
    expect(closed).toHaveBeenCalledTimes(1);
  });
  it('只有匹配 Pong 延长期限，未完成探针不被下一 tick 替换', () => {
    const { socket, connection } = fixture();
    vi.advanceTimersByTime(30000);
    expect(socket.probes).toHaveLength(1);
    socket.emit('pong', Buffer.from('wrong'));
    vi.advanceTimersByTime(15000);
    expect(connection.online).toBe(false);
    expect(socket.terminated).toBe(true);
  });
  it('有效 Pong 维持在线，重复旧 Pong 不能延长下一探针期限', () => {
    const { socket, connection } = fixture();
    vi.advanceTimersByTime(15000);
    socket.emit('pong', socket.probes[0]);
    vi.advanceTimersByTime(15000);
    expect(socket.probes).toHaveLength(2);
    socket.emit('pong', socket.probes[0]);
    vi.advanceTimersByTime(30000);
    expect(connection.online).toBe(false);
  });
  it('通知合并，慢写独立超时且迟到回调不能复活连接', () => {
    const { socket, connection, closed } = fixture();
    socket.blocked = true;
    connection.changed();
    for (let i = 0; i < 1000; i++) connection.changed();
    expect(socket.texts).toHaveLength(3);
    vi.advanceTimersByTime(5000);
    expect(closed).toHaveBeenCalledTimes(1);
    expect(connection.online).toBe(false);
    connection.changed();
    expect(socket.texts).toHaveLength(3);
  });
  it('旧连接的迟到消息不能恢复在线，唤醒后不补发旧心跳', () => {
    const { socket, connection, closed } = fixture();
    connection.stop();
    socket.emit(
      'message',
      Buffer.from(
        JSON.stringify({ type: 'ready', protocol_version: 2, center_id: 'center', registration_id: 'device' }),
      ),
      false,
    );
    expect(connection.online).toBe(false);
    expect(closed).toHaveBeenCalledTimes(1);
    const resumed = fixture();
    const clock = vi.spyOn(performance, 'now').mockReturnValue(60000);
    vi.advanceTimersByTime(250);
    expect(resumed.connection.online).toBe(false);
    expect(resumed.socket.probes).toHaveLength(0);
    clock.mockRestore();
  });
  it('写缓冲超限立即停止，停用通知本身不会关闭连接', () => {
    const { socket, connection } = fixture();
    connection.changed();
    expect(connection.online).toBe(true);
    socket.bufferedAmount = 262145;
    vi.advanceTimersByTime(250);
    expect(connection.online).toBe(false);
  });
});

it('控制消息在业务块之间优先发送，单流排队不绕过写 owner', () => {
  const { socket, connection } = fixture();
  socket.blocked = true;
  connection.sendBinary('one', Buffer.from([1]), () => {});
  connection.sendBinary('two', Buffer.from([2]), () => {});
  connection.send({ type: 'access_revoked', access_id: 'access' });
  socket.blocked = false;
  socket.callbacks.shift()!();
  expect(socket.texts.slice(-3)).toEqual([
    Buffer.from([1]),
    JSON.stringify({ type: 'access_revoked', access_id: 'access' }),
    Buffer.from([2]),
  ]);
  expect(connection.online).toBe(true);
  connection.stop();
});
