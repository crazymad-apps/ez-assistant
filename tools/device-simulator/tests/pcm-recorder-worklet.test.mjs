import { afterAll, describe, expect, it } from "vitest";

const originalGlobals = {
  AudioWorkletProcessor: globalThis.AudioWorkletProcessor,
  registerProcessor: globalThis.registerProcessor,
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

globalThis.AudioWorkletProcessor = class {
  constructor() {
    this.port = new FakePort();
  }
};
globalThis.registerProcessor = (name, constructor) => {
  expect(name).toBe("ez-assistant-pcm-recorder");
  Processor = constructor;
};
await import("../src/web/pcmRecorder.worklet.js");

afterAll(() => {
  restoreGlobal("AudioWorkletProcessor", originalGlobals.AudioWorkletProcessor);
  restoreGlobal("registerProcessor", originalGlobals.registerProcessor);
});

describe("PcmRecorderProcessor", () => {
  it("copies mono render quanta to the control thread and keeps output silent", () => {
    const processor = new Processor({ processorOptions: { maximumSamples: 10 } });
    const output = new Float32Array(4).fill(1);

    expect(processor.process([[Float32Array.of(0.25, -0.5)]], [[output]])).toBe(true);
    expect(Array.from(output)).toEqual([0, 0, 0, 0]);
    expect(processor.port.posted).toHaveLength(1);
    expect(processor.port.posted[0].type).toBe("samples");
    expect(Array.from(processor.port.posted[0].samples)).toEqual([0.25, -0.5]);
  });

  it("truncates at the configured duration and reports the limit once", () => {
    const processor = new Processor({ processorOptions: { maximumSamples: 3 } });
    processor.process([[Float32Array.of(1, 2, 3, 4)]], [[new Float32Array(4)]]);
    processor.process([[Float32Array.of(5, 6)]], [[new Float32Array(4)]]);

    expect(Array.from(processor.port.posted[0].samples)).toEqual([1, 2, 3]);
    expect(processor.port.posted.filter((message) => message.type === "maximum")).toHaveLength(1);
  });

  it("stops after close", () => {
    const processor = new Processor({ processorOptions: { maximumSamples: 10 } });
    processor.port.receive({ type: "close" });
    expect(processor.process([[Float32Array.of(1)]], [[new Float32Array(1)]])).toBe(false);
    expect(processor.port.closed).toBe(true);
  });
});

function restoreGlobal(name, value) {
  if (value === undefined) delete globalThis[name];
  else globalThis[name] = value;
}
