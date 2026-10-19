import type { DeviceConnection } from './connection.js';
import { decodeFrame, streamId, streamWindow, type StreamFrame } from './stream-frame.js';

type Peers = { source: DeviceConnection; target: DeviceConnection };
type Direction = {
  credit: number;
  ingress: number;
  head: boolean;
  ended: boolean;
  writing: boolean;
  queue: StreamFrame[];
  progress: number;
};
type Flow = Peers & {
  kind: 'http' | 'terminal';
  key: string;
  access: string;
  stream: string;
  ready: boolean;
  started: number;
  directions: [Direction, Direction];
};
const direction = (): Direction => ({
  credit: streamWindow,
  ingress: streamWindow,
  head: false,
  ended: false,
  writing: false,
  queue: [],
  progress: performance.now(),
});

/** 访问 owner 下的临时流；每流预留双向窗口，连接排队与在途发送引用同一 Buffer。 */
export class NetworkStreams {
  private readonly flows = new Map<string, Flow>();
  private readonly highest = new Map<string, bigint>();
  private readonly timer = setInterval(() => this.tick(), 250);

  constructor(private readonly route: (access: string, peer: DeviceConnection) => Peers | undefined) {
    this.timer.unref();
  }

  close() {
    clearInterval(this.timer);
    for (const flow of this.flows.values()) this.reset(flow);
    this.highest.clear();
  }

  revoke(access: string) {
    for (const flow of this.flows.values()) if (flow.access === access) this.reset(flow);
    this.highest.delete(access);
  }

  control(peer: DeviceConnection, value: Record<string, unknown>) {
    const access = value.access_id,
      stream = value.stream_id;
    if (typeof access !== 'string' || !streamId(stream)) throw new Error('invalid stream control');
    const key = `${access}/${stream}`;
    if (value.type === 'stream_open') {
      const peers = this.route(access, peer);
      if (!peers) {
        peer.send({ type: 'stream_reset', access_id: access, stream_id: stream, code: 'network_stream_failed' });
        return;
      }
      if (
        peer !== peers.source ||
        (value.kind !== 'http' && value.kind !== 'terminal') ||
        BigInt(stream) <= (this.highest.get(access) ?? 0n)
      )
        throw new Error('invalid stream open');
      this.highest.set(access, BigInt(stream));
      const all = [...this.flows.values()];
      // 每连接 4 MiB 的预留预算比 64 流并发上限更早触发；不能先承诺信用再检查内存。
      if (
        all.length >= 512 ||
        all.filter((f) => f.access === access).length >= 16 ||
        [peers.source, peers.target].some((p) => all.filter((f) => f.source === p || f.target === p).length >= 32)
      ) {
        peer.send({ type: 'stream_reset', access_id: access, stream_id: stream, code: 'network_busy' });
        return;
      }
      const flow: Flow = {
        ...peers,
        kind: value.kind,
        key,
        access,
        stream,
        ready: false,
        started: performance.now(),
        directions: [direction(), direction()],
      };
      this.flows.set(key, flow);
      peers.target.send({ type: 'stream_open', access_id: access, stream_id: stream, kind: flow.kind });
      return;
    }
    const flow = this.flows.get(key);
    if (!flow) return; // 迟到控制不会重建已关闭流。
    if (!this.route(access, peer)) {
      this.reset(flow);
      return;
    }
    if (value.type === 'stream_reset') {
      this.reset(flow);
      return;
    }
    if (value.type === 'stream_ready') {
      if (peer !== flow.target || flow.ready) {
        this.reset(flow);
        return;
      }
      flow.ready = true;
      flow.source.send({ type: 'stream_ready', access_id: access, stream_id: stream });
      return;
    }
    if (value.type !== 'stream_credit' || !Number.isSafeInteger(value.bytes) || (value.bytes as number) <= 0) {
      this.reset(flow);
      return;
    }
    const index = peer === flow.target ? 0 : 1,
      dir = flow.directions[index];
    if (dir.credit + (value.bytes as number) > streamWindow) {
      this.reset(flow);
      return;
    }
    dir.credit += value.bytes as number;
    dir.progress = performance.now();
    this.flush(flow, index);
  }

  binary(peer: DeviceConnection, bytes: Buffer) {
    const frame = decodeFrame(bytes);
    const flow = this.flows.get(`${frame.access}/${frame.stream}`);
    if (!flow) return;
    if (!this.route(frame.access, peer)) {
      this.reset(flow);
      return;
    }
    const index = peer === flow.source ? 0 : 1,
      dir = flow.directions[index];
    if (
      !flow.ready ||
      dir.ended ||
      frame.cost > dir.ingress ||
      (flow.kind === 'http'
        ? !dir.head
          ? frame.kind !== index + 1
          : frame.kind !== 3 && frame.kind !== 4
        : !dir.head
          ? frame.kind !== (index === 0 ? 5 : 6)
          : frame.kind < 6 || frame.kind > 10)
    ) {
      this.reset(flow);
      return;
    }
    dir.head = true;
    dir.ended = frame.kind === 4 || frame.kind === 10;
    dir.ingress -= frame.cost;
    if (!dir.queue.length && !dir.writing) dir.progress = performance.now();
    dir.queue.push(frame);
    this.flush(flow, index);
  }

  private flush(flow: Flow, index: 0 | 1) {
    if (this.flows.get(flow.key) !== flow || !flow.ready) return;
    const dir = flow.directions[index],
      frame = dir.queue[0];
    if (dir.writing || !frame || dir.credit < frame.cost) return;
    dir.queue.shift();
    dir.credit -= frame.cost;
    dir.writing = true;
    const sender = index === 0 ? flow.source : flow.target,
      receiver = index === 0 ? flow.target : flow.source;
    receiver.sendBinary(flow.key, frame.bytes, (ok) => {
      if (this.flows.get(flow.key) !== flow) return;
      dir.writing = false;
      if (!ok) {
        this.reset(flow);
        return;
      }
      dir.ingress += frame.cost;
      dir.progress = performance.now();
      sender.send({ type: 'stream_credit', access_id: flow.access, stream_id: flow.stream, bytes: frame.cost });
      if (flow.directions.every((d) => d.ended && !d.writing && d.queue.length === 0)) {
        this.flows.delete(flow.key);
        return;
      }
      this.flush(flow, index);
    });
  }

  private reset(flow: Flow) {
    if (!this.flows.delete(flow.key)) return;
    for (const peer of [flow.source, flow.target]) {
      peer.discardStream(flow.key);
      peer.send({
        type: 'stream_reset',
        access_id: flow.access,
        stream_id: flow.stream,
        code: 'network_stream_failed',
      });
    }
  }

  private tick() {
    const now = performance.now();
    for (const flow of this.flows.values()) {
      if (
        (!flow.ready && now - flow.started >= 10000) ||
        flow.directions.some((d) => (d.queue.length || d.writing) && now - d.progress >= 30000)
      )
        this.reset(flow);
    }
  }
}
