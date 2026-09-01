import { describe, expect, it } from "vitest";

import {
  BrowserPcmPlayback,
  pcm16ToFloat32,
  StreamingLinearResampler,
  type PcmPlaybackSink,
  type PcmPlaybackSinkEvent,
  type PlaybackMarker,
  type PlaybackStart,
  type PlaybackStatus,
} from "../src/web/pcmPlayback";

describe("pcm16ToFloat32", () => {
  it("decodes little-endian signed PCM without changing sample order", () => {
    const bytes = new Uint8Array(8);
    const view = new DataView(bytes.buffer);
    view.setInt16(0, -32_768, true);
    view.setInt16(2, -16_384, true);
    view.setInt16(4, 0, true);
    view.setInt16(6, 32_767, true);

    expect(Array.from(pcm16ToFloat32(bytes))).toEqual([-1, -0.5, 0, 1]);
  });

  it("honors a Uint8Array byte offset", () => {
    const bytes = Uint8Array.from([99, 99, 0xff, 0x7f, 99]);
    expect(Array.from(pcm16ToFloat32(bytes.subarray(2, 4)))).toEqual([1]);
  });

  it("rejects empty and incomplete samples", () => {
    expect(() => pcm16ToFloat32(new Uint8Array())).toThrow("PCM16");
    expect(() => pcm16ToFloat32(Uint8Array.from([1]))).toThrow("PCM16");
  });
});

describe("StreamingLinearResampler", () => {
  it("keeps interpolation state continuous across transport frame boundaries", () => {
    const resampler = new StreamingLinearResampler(4, 8);
    const first = resampler.process(Float32Array.of(0, 1));
    const second = resampler.process(Float32Array.of(2, 3));

    expect([...first, ...second]).toEqual([0, 0.5, 1, 1.5, 2, 2.5]);
  });

  it("flushes the final source sample and resets for the next segment", () => {
    const resampler = new StreamingLinearResampler(4, 8);
    resampler.process(Float32Array.of(0, 1));
    expect(Array.from(resampler.flush())).toEqual([1, 1]);
    expect(Array.from(resampler.process(Float32Array.of(4, 5)))).toEqual([4, 4.5]);
  });
});

describe("BrowserPcmPlayback", () => {
  it("streams every PCM frame into one sink and marks the segment after its tail", async () => {
    const harness = await createPlaybackHarness();
    harness.player.start(playback("first", 1, 640));
    harness.player.frame(pcmFrame());
    harness.player.frame(pcmFrame());
    harness.player.end({ outputId: "first", streamId: 1, reason: "completed" });

    expect(harness.sink.appended.length).toBe(3);
    expect(harness.sink.appended.reduce((total, samples) => total + samples.length, 0))
      .toBe(1_920);
    expect(harness.sink.markers).toEqual([{ outputId: "first", streamId: 1 }]);
    expect(harness.statuses.at(-1)?.text).toBe("正在播报");
  });

  it("keeps segments in one sink and inserts a 160 ms native-rate gap", async () => {
    const harness = await createPlaybackHarness();
    harness.player.start(playback("first", 1));
    harness.player.frame(pcmFrame());
    harness.player.end({ outputId: "first", streamId: 1, reason: "completed" });
    harness.player.start(playback("second", 2));

    expect(harness.sink.silences).toEqual([7_680]);
    expect(harness.sink.clearCount).toBe(0);

    harness.sink.drain({ outputId: "first", streamId: 1 });
    expect(harness.statuses.at(-1)?.text).not.toBe("播报完成");
    harness.player.frame(pcmFrame());
    harness.player.end({ outputId: "second", streamId: 2, reason: "completed" });
    harness.sink.drain({ outputId: "second", streamId: 2 });
    expect(harness.statuses.at(-1)?.text).toBe("播报完成");
  });

  it("does not report completion until the worklet drains the marker", async () => {
    const harness = await createPlaybackHarness();
    harness.player.start(playback("drain", 1));
    harness.player.frame(pcmFrame());
    harness.player.end({ outputId: "drain", streamId: 1, reason: "completed" });

    expect(harness.statuses.at(-1)?.text).not.toBe("播报完成");
    harness.sink.drain({ outputId: "drain", streamId: 1 });
    expect(harness.statuses.at(-1)?.text).toBe("播报完成");
  });

  it("keeps the remote queue untouched when the local worklet reports overflow", async () => {
    const harness = await createPlaybackHarness();
    harness.player.start(playback("overflow", 1));
    harness.player.frame(pcmFrame());
    harness.sink.emit({ type: "overflow" });

    expect(harness.sink.clearCount).toBe(1);
    expect(harness.statuses.at(-1)?.text).toBe("playback_buffer_overflow");
    expect(harness.player.isActive).toBe(false);
  });

  it("waits for a recovery buffer after underflow without clearing the stream", async () => {
    const harness = await createPlaybackHarness();
    harness.player.start(playback("underflow", 1));
    harness.player.frame(pcmFrame());
    harness.sink.emit({ type: "underflow" });

    expect(harness.sink.clearCount).toBe(0);
    expect(harness.player.isActive).toBe(true);
    expect(harness.statuses.at(-1)?.text).toBe("播报缓冲恢复中");
  });

  it("rejects an incomplete segment locally without marking it complete", async () => {
    const harness = await createPlaybackHarness();
    harness.player.start(playback("short", 1, 640));
    harness.player.frame(pcmFrame());
    harness.player.end({ outputId: "short", streamId: 1, reason: "completed" });

    expect(harness.sink.markers).toEqual([]);
    expect(harness.sink.clearCount).toBe(1);
    expect(harness.statuses.at(-1)?.text).toBe("playback_sample_count_mismatch");
  });

  it("cancels local worklet audio immediately on a real playback cancellation", async () => {
    const harness = await createPlaybackHarness();
    harness.player.start(playback("cancel", 1));
    harness.player.frame(pcmFrame());
    harness.player.end({ outputId: "cancel", streamId: 1, reason: "cancelled" });

    expect(harness.sink.clearCount).toBe(1);
    expect(harness.player.isActive).toBe(false);
    expect(harness.statuses.at(-1)?.text).toBe("播报已停止");
  });
});

class FakePcmSink implements PcmPlaybackSink {
  readonly appended: Float32Array[] = [];
  readonly silences: number[] = [];
  readonly markers: PlaybackMarker[] = [];
  beginCount = 0;
  clearCount = 0;
  closed = false;
  onEvent: ((event: PcmPlaybackSinkEvent) => void) | undefined;

  append(samples: Float32Array): void {
    this.appended.push(samples);
  }

  appendSilence(sampleCount: number): void {
    this.silences.push(sampleCount);
  }

  beginSegment(): void {
    this.beginCount += 1;
  }

  markSegment(marker: PlaybackMarker): void {
    this.markers.push(marker);
  }

  clear(): void {
    this.clearCount += 1;
  }

  close(): void {
    this.closed = true;
  }

  emit(event: PcmPlaybackSinkEvent): void {
    this.onEvent?.(event);
  }

  drain(marker: PlaybackMarker): void {
    this.emit({ type: "marker_drained", marker });
  }
}

class FakeAudioContext {
  readonly sampleRate = 48_000;
  state: "running" | "closed" = "running";

  async resume(): Promise<void> {}

  async close(): Promise<void> {
    this.state = "closed";
  }
}

async function createPlaybackHarness(): Promise<{
  player: BrowserPcmPlayback;
  sink: FakePcmSink;
  statuses: PlaybackStatus[];
}> {
  const context = new FakeAudioContext();
  const sink = new FakePcmSink();
  const statuses: PlaybackStatus[] = [];
  const player = new BrowserPcmPlayback(
    (status) => statuses.push(status),
    {
      createContext: () => context as unknown as AudioContext,
      createSink: async (_audioContext, onEvent) => {
        sink.onEvent = onEvent;
        return sink;
      },
    },
  );
  await player.enable();
  return { player, sink, statuses };
}

function playback(outputId: string, streamId: number, sampleCount = 320): PlaybackStart {
  return {
    outputId,
    runId: "run-test",
    streamId,
    text: outputId,
    sampleCount,
  };
}

function pcmFrame(sampleCount = 320): Uint8Array {
  return new Uint8Array(sampleCount * 2);
}
