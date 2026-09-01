import { describe, expect, it } from "vitest";

import {
  isTerminalProfile,
  terminalProfileDefinition,
} from "../src/node/profiles.js";

describe("terminal capability profiles", () => {
  it.each([
    ["voice_only", false, true, false, true, "audio"],
    ["screen_voice", true, true, true, true, "text_and_audio"],
    ["keyboard_screen", true, false, true, false, "text"],
    ["mixed", true, true, true, true, "text_and_audio"],
  ] as const)("maps %s to one explicit hello capability set", (
    profile,
    inputText,
    inputPcm,
    outputText,
    outputPcm,
    preference,
  ) => {
    const definition = terminalProfileDefinition(profile);
    expect(definition.capabilities).toMatchObject({
      input_text: inputText,
      input_pcm16_16k_mono: inputPcm,
      output_text: outputText,
      output_pcm16_16k_mono: outputPcm,
    });
    expect(definition.outputPreference).toBe(preference);
  });

  it("rejects unknown profile names", () => {
    expect(isTerminalProfile("tablet")).toBe(false);
  });
});
