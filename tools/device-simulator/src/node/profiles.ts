import type { DeviceCapabilities, OutputPreference } from "./protocol.js";

export type TerminalProfile = "voice_only" | "screen_voice" | "keyboard_screen" | "mixed";

export interface TerminalProfileDefinition {
  capabilities: DeviceCapabilities;
  outputPreference: OutputPreference;
}

const PROFILES: Record<TerminalProfile, TerminalProfileDefinition> = {
  voice_only: {
    capabilities: {
      input_text: false,
      input_pcm16_16k_mono: true,
      output_text: false,
      output_pcm16_16k_mono: true,
      playback_cancel: true,
      display_status: false,
      display_transcript: false,
    },
    outputPreference: "audio",
  },
  screen_voice: {
    capabilities: {
      input_text: true,
      input_pcm16_16k_mono: true,
      output_text: true,
      output_pcm16_16k_mono: true,
      playback_cancel: true,
      display_status: true,
      display_transcript: true,
    },
    outputPreference: "text_and_audio",
  },
  keyboard_screen: {
    capabilities: {
      input_text: true,
      input_pcm16_16k_mono: false,
      output_text: true,
      output_pcm16_16k_mono: false,
      playback_cancel: false,
      display_status: true,
      display_transcript: true,
    },
    outputPreference: "text",
  },
  mixed: {
    capabilities: {
      input_text: true,
      input_pcm16_16k_mono: true,
      output_text: true,
      output_pcm16_16k_mono: true,
      playback_cancel: true,
      display_status: true,
      display_transcript: true,
    },
    outputPreference: "text_and_audio",
  },
};

export function terminalProfileDefinition(profile: TerminalProfile): TerminalProfileDefinition {
  const definition = PROFILES[profile];
  return {
    capabilities: { ...definition.capabilities },
    outputPreference: definition.outputPreference,
  };
}

export function isTerminalProfile(value: unknown): value is TerminalProfile {
  return typeof value === "string" && Object.hasOwn(PROFILES, value);
}
