import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

import {
  PCM_HEADER_BYTES,
  PCM_PAYLOAD_BYTES,
  decodeDownlinkPcmFrame,
  encodeUplinkPcmFrame,
} from "../src/node/protocol.js";

const fixture = JSON.parse(readFileSync(
  new URL("../../../docs/resources/device-protocol-v1/fixtures/pcm-v1.json", import.meta.url),
  "utf8",
)) as {
  header_hex: string;
  downlink_header_hex: string;
  stream_id: number;
  sequence: number;
  payload_bytes: number;
};

describe("protocol 1.0 PCM frame", () => {
  it("uses a 16-byte network-order header and opaque PCM payload", () => {
    const payload = Buffer.alloc(fixture.payload_bytes);
    payload.writeInt16LE(-1234, 0);
    const frame = encodeUplinkPcmFrame(fixture.stream_id, fixture.sequence, payload);
    expect(frame).toHaveLength(PCM_HEADER_BYTES + PCM_PAYLOAD_BYTES);
    expect(frame.subarray(0, 16).toString("hex")).toBe(fixture.header_hex);
    expect(frame.readInt16LE(PCM_HEADER_BYTES)).toBe(-1234);
  });

  it("rejects partial audio payloads", () => {
    expect(() => encodeUplinkPcmFrame(1, 0, Buffer.alloc(639))).toThrow(/640 bytes/);
  });

  it("strictly decodes Host downlink PCM frames", () => {
    const frame = Buffer.alloc(PCM_HEADER_BYTES + PCM_PAYLOAD_BYTES);
    Buffer.from(fixture.downlink_header_hex, "hex").copy(frame);
    frame.writeInt16LE(-1234, PCM_HEADER_BYTES);
    const decoded = decodeDownlinkPcmFrame(frame);
    expect(decoded.streamId).toBe(fixture.stream_id);
    expect(decoded.sequence).toBe(fixture.sequence);
    expect(Buffer.from(decoded.payload).readInt16LE(0)).toBe(-1234);
    frame[1] = 1;
    expect(() => decodeDownlinkPcmFrame(frame)).toThrow(/header/);
  });
});
