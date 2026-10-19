import { randomBytes, randomUUID } from 'node:crypto';
import { WebSocket } from 'ws';

const protocol = 2;
const frameLimit = 16 * 1024;
const bufferLimit = 256 * 1024;

/** 单连接只允许一个未完成写与一个合并的通知；超时不依赖写回调。 */
export class DeviceConnection {
  /** 每次连接重新生成；仅由中心用于关联来源，不接受客户端自报。 */
  readonly connectionId = randomUUID();
  online = false;
  private stopped = false;
  private readonly started = performance.now();
  private lastPong = this.started;
  private nextPing = this.started;
  private probe?: Buffer;
  private writingSince?: number;
  private changedPending = false;
  private readonly queued: { text: string; done?: (ok: boolean) => void }[] = [];
  private queuedBytes = 0;
  private requests = 0;
  private readonly data = new Map<string, { bytes: Buffer; done: (ok: boolean) => void }[]>();
  private dataBytes = 0;
  private controls = 0;
  private controlEpoch = performance.now();
  private timer?: ReturnType<typeof setInterval>;

  constructor(
    private readonly ws: WebSocket,
    private readonly center: string,
    readonly id: string | null,
    private readonly closed: () => void,
    private readonly control?: (value: unknown) => Promise<void>,
    private readonly binary?: (bytes: Buffer) => void,
  ) {}

  start() {
    this.ws.on('error', () => this.stop());
    this.ws.on('close', () => this.stop());
    this.ws.on('ping', () => this.stop());
    this.ws.on('pong', (data) => {
      if (this.online && this.probe?.equals(data)) {
        if (performance.now() - this.lastPong >= 45000) return this.stop();
        this.probe = undefined;
        this.lastPong = performance.now();
      }
    });
    this.ws.on('message', (data, binary) => {
      if (this.stopped) return;
      if (binary) {
        if (!this.online || !this.binary) return this.stop();
        try {
          this.binary(Buffer.isBuffer(data) ? data : Buffer.concat(Array.isArray(data) ? data : [Buffer.from(data)]));
        } catch {
          this.stop();
        }
        return;
      }
      if (Buffer.byteLength(data.toString()) > frameLimit) return this.stop();
      try {
        const now = performance.now();
        if (now - this.controlEpoch >= 1000) {
          this.controlEpoch = now;
          this.controls = 0;
        }
        if (++this.controls > 4096) return this.stop();
        const message = JSON.parse(data.toString());
        if (this.online) {
          if (!this.control || this.requests >= 16) return this.stop();
          this.requests++;
          void this.control(message)
            .catch(() => this.stop())
            .finally(() => {
              this.requests--;
            });
          return;
        }
        if (performance.now() - this.started >= 5000) return this.stop();
        if (
          message.type !== 'ready' ||
          message.protocol_version !== protocol ||
          message.center_id !== this.center ||
          message.registration_id !== this.id
        )
          return this.stop();
        this.online = true;
        this.lastPong = performance.now();
        this.changedPending = true;
        // ready 同时承担变更通知：Host 完成连接后始终查询同一登记接口。
        this.flush();
      } catch {
        this.stop();
      }
    });
    this.timer = setInterval(() => this.tick(), 250);
    this.write(
      JSON.stringify({ type: 'hello', protocol_version: protocol, center_id: this.center, registration_id: this.id }),
    );
  }

  send(value: object, done?: (ok: boolean) => void): boolean {
    if (this.stopped) {
      done?.(false);
      return false;
    }
    const text = JSON.stringify(value),
      bytes = Buffer.byteLength(text);
    if (bytes > frameLimit || this.queuedBytes + bytes > bufferLimit) {
      done?.(false);
      this.stop();
      return false;
    }
    this.queued.push({ text, done });
    this.queuedBytes += bytes;
    this.flush();
    return true;
  }

  sendBinary(key: string, bytes: Buffer, done: (ok: boolean) => void) {
    if (this.stopped || this.dataBytes + bytes.length > 4 * 1024 * 1024) {
      done(false);
      return;
    }
    const queue = this.data.get(key) ?? [];
    queue.push({ bytes, done });
    this.data.set(key, queue);
    this.dataBytes += bytes.length;
    this.flush();
  }

  discardStream(key: string) {
    const queue = this.data.get(key);
    this.data.delete(key);
    for (const item of queue ?? []) {
      this.dataBytes -= item.bytes.length;
      item.done(false);
    }
  }

  changed() {
    this.changedPending = true;
    this.flush();
  }

  stop() {
    if (this.stopped) return;
    this.stopped = true;
    this.online = false;
    clearInterval(this.timer);
    this.closed();
    for (const item of this.queued.splice(0)) item.done?.(false);
    this.queuedBytes = 0;
    for (const key of this.data.keys()) this.discardStream(key);
    this.ws.terminate();
  }

  private tick() {
    const now = performance.now();
    if (
      (!this.online && now - this.started >= 5000) ||
      (this.online && now - this.lastPong >= 45000) ||
      (this.writingSince !== undefined && now - this.writingSince >= 5000) ||
      this.ws.bufferedAmount > bufferLimit
    )
      return this.stop();
    if (this.online && !this.probe && this.writingSince === undefined && now >= this.nextPing) {
      this.probe = randomBytes(8);
      this.nextPing = now + 15000; // 唤醒后跳过错过的 tick，不补发心跳。
      this.write(this.probe);
    }
    this.flush();
  }

  private flush() {
    if (this.online && this.writingSince === undefined && !this.stopped && this.queued.length) {
      const item = this.queued.shift()!;
      this.queuedBytes -= Buffer.byteLength(item.text);
      this.write(item.text, item.done);
      return;
    }
    if (this.online && this.changedPending && this.writingSince === undefined && !this.stopped) {
      this.changedPending = false;
      this.write(JSON.stringify({ type: 'registration_changed', protocol_version: protocol }));
      return;
    }
    if (this.online && !this.stopped && this.writingSince === undefined) {
      const entry = this.data.entries().next().value;
      if (!entry) return;
      const [key, queue] = entry,
        item = queue.shift()!;
      this.data.delete(key);
      if (queue.length) this.data.set(key, queue); // 每块后轮转流，控制消息在下一次 flush 优先。
      this.write(
        item.bytes,
        (ok) => {
          this.dataBytes -= item.bytes.length;
          item.done(ok);
        },
        true,
      );
    }
  }

  private write(data: string | Buffer, sent?: (ok: boolean) => void, binary = false) {
    if (this.stopped) return;
    this.writingSince = performance.now();
    const done = (error?: Error) => {
      if (this.stopped) {
        sent?.(false);
        return;
      }
      sent?.(!error);
      this.writingSince = undefined;
      if (error) this.stop();
      else this.flush();
    };
    try {
      if (typeof data === 'string' || binary) this.ws.send(data, done);
      else this.ws.ping(data, false, done);
    } catch {
      sent?.(false);
      this.stop();
    }
  }
}
