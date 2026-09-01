import { randomBytes } from "node:crypto";

export const PROTOCOL_MAJOR = 1;
export const PROTOCOL_MINOR = 0;
export const MAX_CONTROL_MESSAGE_BYTES = 64 * 1024;
export const PCM_HEADER_BYTES = 16;
export const PCM_PAYLOAD_BYTES = 640;

export interface DeviceCapabilities {
  input_text: boolean;
  input_pcm16_16k_mono: boolean;
  output_text: boolean;
  output_pcm16_16k_mono: boolean;
  playback_cancel: boolean;
  display_status: boolean;
  display_transcript: boolean;
}

export type OutputPreference = "text" | "audio" | "text_and_audio";

export interface Envelope<T = unknown> {
  protocol_major: number;
  protocol_minor: number;
  message_id: string;
  type: string;
  payload: T;
}

export function envelope<T>(type: string, payload: T): Envelope<T> {
  return {
    protocol_major: PROTOCOL_MAJOR,
    protocol_minor: PROTOCOL_MINOR,
    message_id: base64Url(randomBytes(12)),
    type,
    payload,
  };
}

export function encodeEnvelope(value: Envelope): string {
  const encoded = JSON.stringify(value);
  if (Buffer.byteLength(encoded) > MAX_CONTROL_MESSAGE_BYTES) {
    throw new Error("control message exceeds 64 KiB");
  }
  return encoded;
}

export function encodeUplinkPcmFrame(
  streamId: number,
  sequence: number,
  payload: Uint8Array,
): Buffer {
  if (!Number.isInteger(streamId) || streamId <= 0 || streamId > 0xffff_ffff) {
    throw new Error("PCM stream id is invalid");
  }
  if (!Number.isInteger(sequence) || sequence < 0 || sequence > 0xffff_ffff) {
    throw new Error("PCM sequence is invalid");
  }
  if (payload.byteLength !== PCM_PAYLOAD_BYTES) {
    throw new Error("PCM payload must contain exactly 640 bytes");
  }
  const frame = Buffer.allocUnsafe(PCM_HEADER_BYTES + PCM_PAYLOAD_BYTES);
  frame[0] = 1;
  frame[1] = 1;
  frame.writeUInt16BE(0, 2);
  frame.writeUInt32BE(streamId, 4);
  frame.writeUInt32BE(sequence, 8);
  frame.writeUInt16BE(PCM_PAYLOAD_BYTES, 12);
  frame.writeUInt16BE(0, 14);
  Buffer.from(payload).copy(frame, PCM_HEADER_BYTES);
  return frame;
}

export interface DownlinkPcmFrame {
  streamId: number;
  sequence: number;
  payload: Uint8Array;
}

export function decodeDownlinkPcmFrame(frame: Uint8Array): DownlinkPcmFrame {
  if (frame.byteLength !== PCM_HEADER_BYTES + PCM_PAYLOAD_BYTES) {
    throw new Error("downlink PCM frame length is invalid");
  }
  const bytes = Buffer.from(frame.buffer, frame.byteOffset, frame.byteLength);
  if (
    bytes[0] !== 1 ||
    bytes[1] !== 2 ||
    bytes.readUInt16BE(2) !== 0 ||
    bytes.readUInt16BE(12) !== PCM_PAYLOAD_BYTES ||
    bytes.readUInt16BE(14) !== 0
  ) {
    throw new Error("downlink PCM frame header is invalid");
  }
  const streamId = bytes.readUInt32BE(4);
  if (streamId === 0) throw new Error("downlink PCM stream id is invalid");
  return {
    streamId,
    sequence: bytes.readUInt32BE(8),
    payload: Uint8Array.from(bytes.subarray(PCM_HEADER_BYTES)),
  };
}

export function decodeEnvelope(text: string): Envelope {
  if (Buffer.byteLength(text) > MAX_CONTROL_MESSAGE_BYTES) {
    throw new Error("control message exceeds 64 KiB");
  }
  const value: unknown = JSON.parse(text);
  if (!isRecord(value)) throw new Error("control envelope must be an object");
  const keys = Object.keys(value).sort().join(",");
  if (keys !== "message_id,payload,protocol_major,protocol_minor,type") {
    throw new Error("control envelope fields are invalid");
  }
  if (
    value.protocol_major !== PROTOCOL_MAJOR ||
    typeof value.protocol_minor !== "number" ||
    typeof value.message_id !== "string" ||
    value.message_id.length === 0 ||
    value.message_id.length > 128 ||
    typeof value.type !== "string" ||
    value.type.length === 0 ||
    value.type.length > 64
  ) {
    throw new Error("control envelope values are invalid");
  }
  return value as unknown as Envelope;
}

export function base64Url(value: Uint8Array): string {
  return Buffer.from(value).toString("base64url");
}

export function fromBase64Url(value: string, expectedLength?: number): Buffer {
  const decoded = Buffer.from(value, "base64url");
  if (base64Url(decoded) !== value || (expectedLength !== undefined && decoded.length !== expectedLength)) {
    throw new Error("invalid base64url value");
  }
  return decoded;
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
