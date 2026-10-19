/** 业务载荷不在中心解码；这里只校验固定外层及消息预算。 */
export const streamWindow = 64 * 1024;
export const streamBlock = 32 * 1024;
export const streamHeader = 30;
export type StreamFrame = { access: string; stream: string; kind: number; cost: number; bytes: Buffer };

export function streamId(value: unknown): value is string {
  return typeof value === 'string' && /^[1-9][0-9]{0,19}$/.test(value) && BigInt(value) <= 0xffffffffffffffffn;
}

export function decodeFrame(bytes: Buffer): StreamFrame {
  if (bytes.length < streamHeader || bytes[0] !== 2) throw new Error('invalid stream frame');
  const kind = bytes.readUInt8(1),
    length = bytes.readUInt32BE(26);
  if (
    kind < 1 ||
    kind > 10 ||
    length > streamBlock ||
    bytes.length !== streamHeader + length ||
    ((kind === 1 || kind === 2) && length > 16384) ||
    ((kind === 4 || kind === 10) && length !== 0) ||
    ((kind === 5 || kind === 6) && length > 16384) ||
    ((kind === 8 || kind === 9) && length > 125)
  )
    throw new Error('invalid stream frame');
  const hex = bytes.subarray(2, 18).toString('hex');
  const access = `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
  const stream = bytes.readBigUInt64BE(18).toString();
  if (!streamId(stream)) throw new Error('invalid stream id');
  return { access, stream, kind, cost: Math.max(256, streamHeader + length), bytes };
}
