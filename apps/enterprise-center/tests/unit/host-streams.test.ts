import { readFileSync } from 'node:fs';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import type { DeviceConnection } from '../../src/host-devices/connection.js';
import { NetworkStreams } from '../../src/host-devices/streams.js';
import { decodeFrame, streamHeader, streamWindow } from '../../src/host-devices/stream-frame.js';

const access = '00112233-4455-4677-8899-aabbccddeeff';
class Peer {
  controls: Record<string, unknown>[] = [];
  data: Buffer[] = [];
  pending: (() => void)[] = [];
  blocked = false;

  send(value: Record<string, unknown>) {
    this.controls.push(value);
  }

  sendBinary(_key: string, bytes: Buffer, done: (ok: boolean) => void) {
    this.data.push(bytes);
    if (this.blocked) this.pending.push(() => done(true));
    else done(true);
  }

  discardStream() {
    this.pending = [];
  }
}
function frame(kind: number, size = 0, stream = '1') {
  const bytes = Buffer.alloc(streamHeader + size);
  bytes[0] = 2;
  bytes[1] = kind;
  Buffer.from(access.replaceAll('-', ''), 'hex').copy(bytes, 2);
  bytes.writeBigUInt64BE(BigInt(stream), 18);
  bytes.writeUInt32BE(size, 26);
  return bytes;
}
const active: NetworkStreams[] = [];
function fixture(kind: 'http' | 'terminal' = 'http') {
  const a = new Peer(),
    b = new Peer();
  const source = a as unknown as DeviceConnection,
    target = b as unknown as DeviceConnection;
  const route = vi.fn((_id: string, peer: DeviceConnection) => {
    if (peer !== source && peer !== target) throw new Error('foreign peer');
    return { source, target };
  });
  const streams = new NetworkStreams(route);
  active.push(streams);
  const control = (peer: DeviceConnection, type: string, extra = {}) =>
    streams.control(peer, { type, access_id: access, stream_id: '1', ...extra });
  control(source, 'stream_open', { kind });
  control(target, 'stream_ready');
  return { a, b, source, target, streams, control, route };
}
beforeEach(() => vi.useFakeTimers({ toFake: ['performance', 'setInterval', 'clearInterval'] }));
afterEach(() => {
  for (const streams of active.splice(0)) streams.close();
  vi.useRealTimers();
});

it('shares a u64-exact fixture with Rust and rejects truncated/oversized frames', () => {
  const fixture = JSON.parse(readFileSync('../runtime-host/tests/fixtures/network-stream-v2.json', 'utf8'));
  const bytes = Buffer.from(fixture.hex, 'hex');
  expect(decodeFrame(bytes)).toMatchObject({ access: fixture.access, stream: fixture.stream, kind: fixture.kind });
  expect([...bytes.subarray(streamHeader)]).toEqual(fixture.payload);
  expect(() => decodeFrame(bytes.subarray(1))).toThrow();
  expect(() => decodeFrame(frame(1, 16385))).toThrow();
  expect(() => decodeFrame(frame(3, 32769))).toThrow();
  expect(() => decodeFrame(frame(4, 1))).toThrow();
});

it('forwards opaque head and body in order; request FIN does not end the response', () => {
  const { a, b, source, target, streams, route } = fixture();
  streams.binary(source, frame(1, 2));
  streams.binary(source, frame(3, 10));
  streams.binary(source, frame(4));
  streams.binary(target, frame(2, 2));
  streams.binary(target, frame(3, 20));
  streams.binary(target, frame(4));
  expect(b.data.map((v) => v[1])).toEqual([1, 3, 4]);
  expect(a.data.map((v) => v[1])).toEqual([2, 3, 4]);
  route.mockClear();
  streams.revoke(access);
  streams.binary(source, frame(3, 10));
  expect(route).not.toHaveBeenCalled(); // revoked late bytes do not close the shared connection
});

it('slow receiver pauses at its credit; returned credit resumes only that direction', () => {
  const { a, b, source, target, streams, control } = fixture();
  streams.binary(source, frame(1, 2));
  streams.binary(source, frame(3, 32768));
  streams.binary(source, frame(3, streamWindow - 256 - 32768 - 60));
  streams.binary(source, frame(3, 100));
  expect(b.data).toHaveLength(3);
  streams.binary(target, frame(2, 2));
  expect(a.data).toHaveLength(1);
  control(target, 'stream_credit', { bytes: 256 });
  expect(b.data).toHaveLength(4);
  control(target, 'stream_credit', { bytes: streamWindow + 1 });
  expect(a.controls.at(-1)?.type).toBe('stream_reset');
});

it('reset discards queued bytes, bounds access concurrency and rejects foreign peers', () => {
  const { a, b, source, streams, control } = fixture();
  b.blocked = true;
  streams.binary(source, frame(1, 2));
  streams.binary(source, frame(3, 32768));
  expect(() => streams.binary(new Peer() as unknown as DeviceConnection, frame(2))).toThrow();
  control(source, 'stream_reset');
  expect(b.pending).toHaveLength(0);
  for (let n = 2; n < 19; n++) control(source, 'stream_open', { kind: 'http', stream_id: String(n) });
  expect(a.controls.at(-1)).toMatchObject({ type: 'stream_reset', code: 'network_busy' });
  vi.advanceTimersByTime(10000);
  control(source, 'stream_open', { kind: 'http', stream_id: '19' });
  expect(b.controls.at(-1)).toMatchObject({ type: 'stream_open', stream_id: '19' });
});

it('terminal messages preserve boundaries and close independently of HTTP streams', () => {
  const { a, b, source, target, streams, control } = fixture('terminal');
  streams.binary(source, frame(5, 20));
  streams.binary(target, frame(6, 20));
  for (const kind of [6, 7, 8, 9]) {
    streams.binary(source, frame(kind, 10));
    streams.binary(target, frame(kind, 10));
  }
  streams.binary(source, frame(10));
  streams.binary(target, frame(10));
  expect(b.data.map((v) => v[1])).toEqual([5, 6, 7, 8, 9, 10]);
  expect(a.data.map((v) => v[1])).toEqual([6, 6, 7, 8, 9, 10]);
  control(source, 'stream_open', { kind: 'terminal', stream_id: '2' });
  control(target, 'stream_ready', { stream_id: '2' });
  streams.binary(source, frame(1, 0, '2'));
  expect(a.controls.at(-1)?.type).toBe('stream_reset');
  for (const [kind, size] of [
    [5, 16385],
    [6, 16385],
    [7, 32769],
    [8, 126],
    [9, 126],
    [10, 1],
  ])
    expect(() => decodeFrame(frame(kind!, size!))).toThrow();
});
