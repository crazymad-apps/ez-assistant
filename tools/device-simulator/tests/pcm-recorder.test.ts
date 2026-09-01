import { describe, expect, it } from "vitest";

import { encodeProtocolPcm } from "../src/web/pcmRecorder.js";

describe("browser microphone PCM conversion", () => {
  it("downsamples mono audio to framed PCM16 little-endian", () => {
    const input = Float32Array.from({ length: 48_000 }, (_, index) => (
      Math.sin(2 * Math.PI * 440 * index / 48_000) * 0.5
    ));
    const pcm = encodeProtocolPcm(input, 48_000);
    expect(pcm).toHaveLength(16_000 * 2);
    expect(pcm.byteLength % 640).toBe(0);
    expect(new DataView(pcm.buffer).getInt16(20, true)).not.toBe(0);
  });

  it("pads a partial final frame with silence", () => {
    const pcm = encodeProtocolPcm(new Float32Array(1_000).fill(0.25), 16_000);
    expect(pcm).toHaveLength(4 * 640);
    expect(new DataView(pcm.buffer).getInt16(2_000, true)).toBe(0);
  });

  it("rejects an invalid input sample rate", () => {
    expect(() => encodeProtocolPcm(new Float32Array(320), 0)).toThrow(/采样率/);
  });
});
