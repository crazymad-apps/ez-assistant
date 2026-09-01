import { afterAll, describe, expect, it } from "vitest";

const originalGlobals = {
  AudioWorkletProcessor: globalThis.AudioWorkletProcessor,
  registerProcessor: globalThis.registerProcessor,
  sampleRate: globalThis.sampleRate,
};
let Processor;

class FakePort {
  onmessage;
  posted = [];
  closed = false;

  postMessage(message) {
    this.posted.push(message);
  }

  close() {
    this.closed = true;
  }

  receive(message) {
    this.onmessage?.({ data: message });
  }
}

globalThis.sampleRate = 48_000;
globalThis.AudioWorkletProcessor = class {
  constructor() {
    this.port = new FakePort();
  }
};
globalThis.registerProcessor = (name, constructor) => {
  expect(name).toBe("ez-assistant-pcm-playback");
  Processor = constructor;
};
await import("../src/web/pcmPlayback.worklet.js");

afterAll(() => {
  restoreGlobal("AudioWorkletProcessor", originalGlobals.AudioWorkletProcessor);
  restoreGlobal("registerProcessor", originalGlobals.registerProcessor);
  restoreGlobal("sampleRate", originalGlobals.sampleRate);
});

describe("PcmPlaybackProcessor", () => {
  it("renders queued chunks as one continuous output and drains its marker", () => {
    const processor = createProcessor();
    processor.port.receive({ type: "samples", samples: Float32Array.of(1, 2) });
    processor.port.receive({ type: "samples", samples: Float32Array.of(3, 4) });
    processor.port.receive({
      type: "marker",
      marker: { outputId: "output-1", streamId: 1 },
    });
    const output = new Float32Array(8);

    expect(processor.process([], [[output]])).toBe(true);
    expect(Array.from(output.slice(0, 4))).toEqual([1, 2, 3, 4]);
    expect(processor.port.posted).toContainEqual({
      type: "marker_drained",
      marker: { outputId: "output-1", streamId: 1 },
    });
  });

  it("waits for the recovery threshold after a real underflow", () => {
    const processor = createProcessor();
    processor.port.receive({ type: "samples", samples: Float32Array.of(1, 2, 3, 4) });
    processor.process([], [[new Float32Array(8)]]);
    expect(processor.port.posted).toContainEqual({ type: "underflow" });

    processor.port.receive({ type: "samples", samples: Float32Array.of(5, 6) });
    const waiting = new Float32Array(4);
    processor.process([], [[waiting]]);
    expect(Array.from(waiting)).toEqual([0, 0, 0, 0]);

    processor.port.receive({ type: "samples", samples: Float32Array.of(7) });
    const resumed = new Float32Array(4);
    processor.process([], [[resumed]]);
    expect(Array.from(resumed.slice(0, 3))).toEqual([5, 6, 7]);
  });

  it("reports overflow without silently evicting previously queued samples", () => {
    const processor = createProcessor({ maxBufferSamples: 4 });
    processor.port.receive({ type: "samples", samples: Float32Array.of(1, 2, 3, 4) });
    processor.port.receive({ type: "samples", samples: Float32Array.of(5) });

    expect(processor.port.posted).toEqual([{ type: "overflow" }]);
    const output = new Float32Array(4);
    processor.process([], [[output]]);
    expect(Array.from(output)).toEqual([1, 2, 3, 4]);
  });
});

function createProcessor(overrides = {}) {
  return new Processor({
    processorOptions: {
      initialBufferSamples: 4,
      recoveryBufferSamples: 3,
      maxBufferSamples: 100,
      fadeSamples: 1,
      ...overrides,
    },
  });
}

function restoreGlobal(name, value) {
  if (value === undefined) delete globalThis[name];
  else globalThis[name] = value;
}
