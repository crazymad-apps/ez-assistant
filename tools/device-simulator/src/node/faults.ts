export type SimulatorFault =
  | "corrupt_next_auth_signature"
  | "next_protocol_major_mismatch"
  | "duplicate_next_text_envelope"
  | "invalid_next_pcm_sequence"
  | "disconnect_after_next_input"
  | "ignore_next_ping"
  | "pause_read_5s"
  | "unsupported_output_preference"
  | "duplicate_playback_cancel";

const FAULTS = new Set<SimulatorFault>([
  "corrupt_next_auth_signature",
  "next_protocol_major_mismatch",
  "duplicate_next_text_envelope",
  "invalid_next_pcm_sequence",
  "disconnect_after_next_input",
  "ignore_next_ping",
  "pause_read_5s",
  "unsupported_output_preference",
  "duplicate_playback_cancel",
]);

export function isSimulatorFault(value: unknown): value is SimulatorFault {
  return typeof value === "string" && FAULTS.has(value as SimulatorFault);
}

export function isImmediateFault(fault: SimulatorFault): boolean {
  return fault === "pause_read_5s"
    || fault === "unsupported_output_preference"
    || fault === "duplicate_playback_cancel";
}
