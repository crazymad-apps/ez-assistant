export type RawRecord = Record<string, unknown>;

export type FleetDeviceItem = Readonly<{
  device_id: string;
  zone: string;
  group_id: string;
  chip: string;
  vp_version: number;
  vad_state: string;
  online: boolean;
  rssi: string;
  volume: string;
  battery_pct: string;
  temperature_c: string;
  free_int: string;
  capabilities: ReadonlyArray<string>;
}>;

export type MacWhitelistEntry = Readonly<{
  mac: string;
  group_id: string;
  zone: string;
  label: string;
  enabled: boolean;
}>;

export type AclRuleEntry = Readonly<{
  rule_id: string;
  agent_id: string;
  group_id: string;
  zone_pattern: string;
  device_id: string;
  capability_pattern: string;
  min_speaker_role: number;
  allow: boolean;
}>;

export type WakeMode = "wake_word" | "wake_word_plus_vp" | "voiceprint_direct";

export type DeviceWakeConfig = Readonly<{
  mode: WakeMode;
  wake_word: string;
  wake_pinyin: string;
  vp_threshold: number;
  min_speech_ms: number;
  cooldown_ms: number;
  vp_wake_chime: boolean;
  speech_end_silence_ms: number;
  turn_commit_delay_ms: number;
}>;

export type DeviceWakeSnapshot = Readonly<{
  wake: DeviceWakeConfig;
  enrolled_speaker_count: number;
  multinet_ready: boolean;
}>;

export type CapturedVoiceprintSample = Readonly<{
  embedding: number[];
  voiced_ms: number;
  snr_db: number;
  rms: number;
  duration_ms: number;
  label: string;
}>;

export type SpeakerGroupEntry = Readonly<{
  speaker_id: string;
  display_name: string;
  uid: string;
  agent_id: string;
  group_id: string;
  role: number;
  tts_voice: string;
  prompt: string;
  version: number;
  template_count: number;
}>;

export function pickString(obj: unknown, ...keys: ReadonlyArray<string>): string {
  if (!obj || typeof obj !== "object") return "";
  const record = obj as RawRecord;
  for (const key of keys) {
    const val = record[key];
    if (typeof val === "string") return val;
    if (typeof val === "number") return String(val);
  }
  return "";
}

export function pickNumber(obj: unknown, fallback: number, ...keys: ReadonlyArray<string>): number {
  if (!obj || typeof obj !== "object") return fallback;
  const record = obj as RawRecord;
  for (const key of keys) {
    const val = record[key];
    if (typeof val === "number" && Number.isFinite(val)) return val;
    if (typeof val === "string" && val.trim() !== "") {
      const parsed = Number(val);
      if (Number.isFinite(parsed)) return parsed;
    }
  }
  return fallback;
}

export function pickBool(obj: unknown, fallback: boolean, ...keys: ReadonlyArray<string>): boolean {
  if (!obj || typeof obj !== "object") return fallback;
  const record = obj as RawRecord;
  for (const key of keys) {
    const val = record[key];
    if (typeof val === "boolean") return val;
  }
  return fallback;
}

export function parseFleetDevices(payload: unknown): FleetDeviceItem[] {
  const list = Array.isArray((payload as RawRecord | null)?.devices)
    ? ((payload as RawRecord).devices as unknown[])
    : [];
  return list.map((raw) => {
    const item = (raw && typeof raw === "object" ? raw : {}) as RawRecord;
    const hello = (item.hello && typeof item.hello === "object" ? item.hello : {}) as RawRecord;
    const shadow = (item.shadow && typeof item.shadow === "object" ? item.shadow : {}) as RawRecord;
    const rawCaps = Array.isArray(hello.capabilities) ? hello.capabilities : [];
    const capabilities = rawCaps
      .map((cap) => pickString(cap, "name"))
      .filter((name) => name.length > 0);
    return {
      device_id:
        pickString(hello, "deviceId", "device_id") ||
        pickString(shadow, "deviceId", "device_id") ||
        "-",
      zone: pickString(hello, "zone") || pickString(shadow, "zone") || "-",
      group_id:
        pickString(hello, "groupId", "group_id") ||
        pickString(shadow, "groupId", "group_id") ||
        "-",
      chip: pickString(hello, "chip") || "-",
      vp_version: pickNumber(hello, 0, "vpVersion", "vp_version"),
      vad_state: pickString(shadow, "vadState", "vad_state") || "idle",
      online: pickBool(shadow, false, "online"),
      rssi: shadow.rssi !== undefined && shadow.rssi !== null ? String(shadow.rssi) : "-",
      volume: shadow.volume !== undefined && shadow.volume !== null ? String(shadow.volume) : "-",
      battery_pct:
        shadow.battery_pct !== undefined && shadow.battery_pct !== null
          ? `${String(shadow.battery_pct)}%`
          : "",
      temperature_c:
        shadow.temperature_c !== undefined && shadow.temperature_c !== null
          ? `${String(shadow.temperature_c)}°C`
          : "",
      free_int:
        shadow.free_int !== undefined && shadow.free_int !== null
          ? `${Math.round(Number(shadow.free_int) / 1024)}KB`
          : "",
      capabilities,
    };
  });
}

export function parseMacEntries(payload: unknown): MacWhitelistEntry[] {
  const items = Array.isArray((payload as RawRecord | null)?.items)
    ? ((payload as RawRecord).items as unknown[])
    : [];
  return items.map((raw) => ({
    mac: pickString(raw, "mac"),
    group_id: pickString(raw, "groupId", "group_id"),
    zone: pickString(raw, "zone"),
    label: pickString(raw, "label"),
    enabled: pickBool(raw, false, "enabled"),
  }));
}

export function parseAclRules(payload: unknown): AclRuleEntry[] {
  const rules = Array.isArray((payload as RawRecord | null)?.rules)
    ? ((payload as RawRecord).rules as unknown[])
    : [];
  return rules.map((raw) => ({
    rule_id: pickString(raw, "ruleId", "rule_id"),
    agent_id: pickString(raw, "agentId", "agent_id"),
    group_id: pickString(raw, "groupId", "group_id") || "*",
    zone_pattern: pickString(raw, "zonePattern", "zone_pattern") || "*",
    device_id: pickString(raw, "deviceId", "device_id"),
    capability_pattern: pickString(raw, "capabilityPattern", "capability_pattern") || "*",
    min_speaker_role: pickNumber(raw, 1, "minSpeakerRole", "min_speaker_role"),
    allow: pickBool(raw, false, "allow"),
  }));
}

export function parseSpeakers(payload: unknown): SpeakerGroupEntry[] {
  const speakers = Array.isArray((payload as RawRecord | null)?.speakers)
    ? ((payload as RawRecord).speakers as unknown[])
    : [];
  return speakers.map((raw) => ({
    speaker_id: pickString(raw, "speakerId", "speaker_id"),
    display_name: pickString(raw, "displayName", "display_name"),
    uid: pickString(raw, "uid"),
    agent_id: pickString(raw, "agentId", "agent_id"),
    group_id: pickString(raw, "groupId", "group_id"),
    role: pickNumber(raw, 1, "role"),
    tts_voice: pickString(raw, "ttsVoice", "tts_voice"),
    prompt: pickString(raw, "prompt"),
    version: pickNumber(raw, 0, "version"),
    template_count: pickNumber(raw, 1, "template_count", "templateCount"),
  }));
}

export function parseWakeSnapshot(payload: unknown): DeviceWakeSnapshot {
  const root = (payload && typeof payload === "object" ? payload : {}) as RawRecord;
  const rawWake = (root.wake && typeof root.wake === "object" ? root.wake : {}) as RawRecord;
  const rawMode = pickString(rawWake, "mode");
  const mode: WakeMode =
    rawMode === "wake_word_plus_vp" || rawMode === "voiceprint_direct" ? rawMode : "wake_word";
  return {
    wake: {
      mode,
      wake_word: pickString(rawWake, "wake_word", "wakeWord") || "你好小元",
      wake_pinyin: pickString(rawWake, "wake_pinyin", "wakePinyin") || "ni hao xiao yuan",
      vp_threshold: pickNumber(rawWake, 0.78, "vp_threshold", "vpThreshold"),
      min_speech_ms: pickNumber(rawWake, 700, "min_speech_ms", "minSpeechMs"),
      cooldown_ms: pickNumber(rawWake, 1500, "cooldown_ms", "cooldownMs"),
      vp_wake_chime: pickBool(rawWake, false, "vp_wake_chime", "vpWakeChime"),
      speech_end_silence_ms: pickNumber(
        rawWake,
        500,
        "speech_end_silence_ms",
        "speechEndSilenceMs",
      ),
      turn_commit_delay_ms: pickNumber(
        rawWake,
        0,
        "turn_commit_delay_ms",
        "turnCommitDelayMs",
      ),
    },
    enrolled_speaker_count: pickNumber(root, 0, "enrolled_speaker_count", "enrolledSpeakerCount"),
    multinet_ready: pickBool(root, true, "multinet_ready", "multinetReady"),
  };
}

export function normalizeConsolePath(apiPrefix: string, suffix: string): string {
  const cleanPrefix = apiPrefix.replace(/^\/+/, "").replace(/\/+$/, "");
  const cleanSuffix = suffix.replace(/^\/+/, "");
  return cleanPrefix ? `${cleanPrefix}/${cleanSuffix}` : cleanSuffix;
}

export function extractMediaRelativePath(mediaUrl: string): string | null {
  const trimmed = mediaUrl.trim();
  if (!trimmed) return null;
  if (trimmed.startsWith("/")) {
    return trimmed.replace(/^\/+/, "");
  }
  try {
    const parsed = new URL(trimmed);
    const relative = `${parsed.pathname.replace(/^\/+/, "")}${parsed.search}`;
    return relative || null;
  } catch {
    return null;
  }
}

export function roleLabel(role: number): string {
  if (role === 1) return "Owner (1)";
  if (role === 2) return "Family (2)";
  return `Guest (${role})`;
}
