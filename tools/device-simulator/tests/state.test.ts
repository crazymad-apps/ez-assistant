import { describe, expect, it } from "vitest";

import { SimulatorState } from "../src/node/state.js";

describe("simulator interaction projection", () => {
  it("keeps the stable client input identity while Runtime ids arrive later", () => {
    const state = new SimulatorState("keyboard_screen");
    state.beginTextInput("client-input-1", "hello controller");
    expect(state.snapshot()).toMatchObject({
      phase: "processing",
      currentInteraction: {
        clientInputId: "client-input-1",
        submittedText: "hello controller",
      },
    });

    state.patch({
      phase: "accepted_or_queued",
      currentInteraction: {
        ...state.snapshot().currentInteraction!,
        inputId: "input-1",
        runId: "run-1",
        queueState: "accepted",
      },
    });
    expect(state.snapshot().currentInteraction).toMatchObject({
      clientInputId: "client-input-1",
      inputId: "input-1",
      runId: "run-1",
    });
  });

  it("changes declared capabilities only while disconnected", () => {
    const state = new SimulatorState("mixed");
    state.patch({ phase: "disconnected" });
    state.setTerminalProfile("voice_only");
    expect(state.snapshot()).toMatchObject({
      terminalProfile: "voice_only",
      outputPreference: "audio",
      declaredCapabilities: {
        input_text: false,
        input_pcm16_16k_mono: true,
        output_text: false,
        output_pcm16_16k_mono: true,
      },
    });

    state.patch({ phase: "idle" });
    expect(() => state.setTerminalProfile("keyboard_screen")).toThrow(/先断开/);
  });

  it("keeps continuous speech segments in one logical interaction", () => {
    const state = new SimulatorState("mixed");
    state.beginSpeechInput("segment-1");
    state.beginSpeechInput("segment-2");
    state.beginSpeechInput("segment-2");

    expect(state.snapshot().currentInteraction).toEqual({
      clientInputId: "segment-1",
      submittedText: "语音输入",
      segmentClientInputIds: ["segment-1", "segment-2"],
    });

    state.patch({
      currentInteraction: {
        ...state.snapshot().currentInteraction!,
        inputId: "input-1",
        runId: "run-1",
      },
    });
    state.beginSpeechInput("segment-3");
    expect(state.snapshot().currentInteraction).toEqual({
      clientInputId: "segment-3",
      submittedText: "语音输入",
      segmentClientInputIds: ["segment-3"],
    });
  });
});
