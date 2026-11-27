import { observer } from "mobx-react-lite";
import { useEffect, useId, useRef, useState } from "react";
import type {
  AsrProfileSnapshot,
  SpeechProviderKind,
  TtsProfileSnapshot,
  UpdateTtsConfigurationInput,
} from "@ez-assistant/protocol";
import { CollapsibleSection } from "../../../../components/CollapsibleSection";
import { InlineIconButton } from "../../../../components/InlineIconButton";
import { ProviderIcon } from "../../../../components/ProviderIcon";
import { SelectionPopover, type SelectionOption } from "../../../../components/SelectionPopover";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { base64ToWavBlob } from "../DeviceConsole/wavRecorder";
import {
  CUSTOM_MODEL_SENTINEL,
  CUSTOM_VOICE_SENTINEL,
  TTS_PRESETS,
  defaultVoiceForModel,
  detectAsrPreset,
  detectTtsPreset,
  isSharedSameCloudProvider,
  ttsPresetVoices,
  type TtsPresetId,
} from "./presets";
import styles from "./index.module.scss";

type TtsDraft = {
  name: string;
  preset: TtsPresetId;
  provider: SpeechProviderKind;
  endpoint: string;
  model: string;
  voice: string;
  custom_voice: boolean;
  timeout_ms: string;
  credential: string;
  reuse_asr: boolean;
};

type TtsTestState =
  | Readonly<{ kind: "idle" }>
  | Readonly<{ kind: "testing" }>
  | Readonly<{ kind: "ok"; audio_url: string; elapsed_ms: number; duration_ms: number }>
  | Readonly<{ kind: "error"; message: string }>;

function shortLabel(label: string): string {
  return label.split(" (")[0] ?? label;
}

export function defaultTtsProfileName(preset: TtsPresetId, voice: string, model: string): string {
  const base = shortLabel(TTS_PRESETS[preset].label);
  const label = voice.trim() || model.trim();
  return label ? `${base} · ${label}` : base;
}

function parseTimeoutMs(raw: string): number {
  const parsed = Number(raw);
  if (!Number.isFinite(parsed) || parsed < 1) return 30000;
  return Math.min(120000, Math.round(parsed));
}

function errorText(error: unknown, fallback: string): string {
  return error instanceof Error && error.message.trim() ? error.message : fallback;
}

function findMatchingAsrProfile(
  preset: TtsPresetId,
  asrProfiles: ReadonlyArray<AsrProfileSnapshot>,
  activeAsrId: string | null,
): AsrProfileSnapshot | null {
  const matches = asrProfiles.filter((item) => {
    if (!item.credential_configured) return false;
    const asrPreset = detectAsrPreset(item.provider, item.endpoint);
    return isSharedSameCloudProvider(asrPreset, preset);
  });
  if (matches.length === 0) return null;
  return matches.find((item) => item.id === activeAsrId) ?? matches[0] ?? null;
}

function initialDraft(
  profile: TtsProfileSnapshot | null,
  asrProfiles: ReadonlyArray<AsrProfileSnapshot>,
  activeAsrId: string | null,
): TtsDraft {
  if (profile) {
    const preset = detectTtsPreset(profile.provider, profile.endpoint);
    const meta = TTS_PRESETS[preset];
    const model = profile.model || meta.default_model;
    const voice = profile.voice || defaultVoiceForModel(preset, model);
    const knownVoices = ttsPresetVoices(preset, model);
    return {
      name: profile.name,
      preset,
      provider: meta.provider,
      endpoint: profile.endpoint || meta.endpoint,
      model,
      voice,
      custom_voice: Boolean(voice) && !knownVoices.some((option) => option.value === voice),
      timeout_ms: String(profile.timeout_ms),
      credential: "",
      reuse_asr: false,
    };
  }
  const meta = TTS_PRESETS.dashscope;
  const matchingAsr = findMatchingAsrProfile("dashscope", asrProfiles, activeAsrId);
  return {
    name: defaultTtsProfileName("dashscope", meta.default_voice, meta.default_model),
    preset: "dashscope",
    provider: meta.provider,
    endpoint: meta.endpoint,
    model: meta.default_model,
    voice: meta.default_voice,
    custom_voice: false,
    timeout_ms: "30000",
    credential: "",
    reuse_asr: Boolean(matchingAsr),
  };
}

function toTtsInput(draft: TtsDraft, reuseAsr: boolean): UpdateTtsConfigurationInput {
  return {
    enabled: true,
    provider: draft.provider,
    endpoint: draft.endpoint.trim(),
    model: draft.model.trim(),
    voice: draft.voice.trim(),
    timeout_ms: parseTimeoutMs(draft.timeout_ms),
    credential: !reuseAsr && draft.credential.trim() ? draft.credential.trim() : undefined,
    clear_credential: false,
    reuse_asr_credential: reuseAsr,
  };
}

export const TtsProfileEditor = observer(function TtsProfileEditor(props: Readonly<{
  profile: TtsProfileSnapshot | null;
  is_active: boolean;
  default_activate: boolean;
  asr_profiles: ReadonlyArray<AsrProfileSnapshot>;
  active_asr_id: string | null;
  on_close: () => void;
}>) {
  const gateway = useRootStore().device_gateway;
  const secretId = useId();
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const objectUrlRef = useRef<string | null>(null);
  const playRafRef = useRef<number | null>(null);

  const [draft, setDraft] = useState<TtsDraft>(() =>
    initialDraft(props.profile, props.asr_profiles, props.active_asr_id),
  );
  const [nameTouched, setNameTouched] = useState<boolean>(Boolean(props.profile));
  const [activate, setActivate] = useState<boolean>(props.is_active || props.default_activate);
  const [providerOpen, setProviderOpen] = useState(false);
  const [modelOpen, setModelOpen] = useState(false);
  const [voiceOpen, setVoiceOpen] = useState(false);
  const [showSecret, setShowSecret] = useState(false);
  const [previewText, setPreviewText] = useState("你好，这是语音合成测试。");
  const [ttsTest, setTtsTest] = useState<TtsTestState>({ kind: "idle" });

  useEffect(() => () => {
    if (playRafRef.current !== null) {
      cancelAnimationFrame(playRafRef.current);
      playRafRef.current = null;
    }
    if (objectUrlRef.current) {
      URL.revokeObjectURL(objectUrlRef.current);
      objectUrlRef.current = null;
    }
  }, []);

  const busy = gateway.pending_action !== null || gateway.speech_loading;
  const testBusy = ttsTest.kind === "testing";
  const meta = TTS_PRESETS[draft.preset];
  const matchingAsr = findMatchingAsrProfile(draft.preset, props.asr_profiles, props.active_asr_id);
  const effectiveReuseAsr = Boolean(matchingAsr) && draft.reuse_asr;

  const providerOptions: ReadonlyArray<SelectionOption<TtsPresetId>> = Object.values(TTS_PRESETS).map((item) => ({
    value: item.id,
    label: item.label,
    description: item.description,
    icon: <ProviderIcon name={item.icon} size={18} />,
  }));

  const modelOptions: ReadonlyArray<SelectionOption<string>> = [
    ...meta.models,
    { value: CUSTOM_MODEL_SENTINEL, label: "自定义模型 ID" },
  ];
  const modelSelected = meta.models.some((option) => option.value === draft.model)
    ? draft.model
    : CUSTOM_MODEL_SENTINEL;

  const voices = ttsPresetVoices(draft.preset, draft.model);
  const voiceOptions: ReadonlyArray<SelectionOption<string>> = [
    ...voices,
    { value: CUSTOM_VOICE_SENTINEL, label: "自定义音色 ID" },
  ];
  const voiceSelected = draft.custom_voice || !voices.some((option) => option.value === draft.voice)
    ? CUSTOM_VOICE_SENTINEL
    : draft.voice;

  function handlePresetChange(nextPreset: TtsPresetId) {
    setProviderOpen(false);
    const nextMeta = TTS_PRESETS[nextPreset];
    const nextMatching = findMatchingAsrProfile(nextPreset, props.asr_profiles, props.active_asr_id);
    setDraft((prev) => ({
      ...prev,
      preset: nextPreset,
      provider: nextMeta.provider,
      endpoint: nextMeta.endpoint,
      model: nextMeta.default_model,
      voice: nextMeta.default_voice,
      custom_voice: false,
      reuse_asr: Boolean(nextMatching) && (!props.profile || prev.reuse_asr),
      name: nameTouched ? prev.name : defaultTtsProfileName(nextPreset, nextMeta.default_voice, nextMeta.default_model),
    }));
    setTtsTest({ kind: "idle" });
  }

  function handleModelChange(nextModel: string) {
    setModelOpen(false);
    if (nextModel === CUSTOM_MODEL_SENTINEL) return;
    setDraft((prev) => {
      const nextVoices = ttsPresetVoices(prev.preset, nextModel);
      const stillValid = !prev.custom_voice && nextVoices.some((option) => option.value === prev.voice);
      const nextVoice = stillValid ? prev.voice : defaultVoiceForModel(prev.preset, nextModel);
      return {
        ...prev,
        model: nextModel,
        voice: nextVoice,
        custom_voice: false,
        name: nameTouched ? prev.name : defaultTtsProfileName(prev.preset, nextVoice, nextModel),
      };
    });
  }

  function handleVoiceChange(nextVoice: string) {
    setVoiceOpen(false);
    if (nextVoice === CUSTOM_VOICE_SENTINEL) {
      setDraft((prev) => ({ ...prev, custom_voice: true }));
      return;
    }
    setDraft((prev) => ({
      ...prev,
      voice: nextVoice,
      custom_voice: false,
      name: nameTouched ? prev.name : defaultTtsProfileName(prev.preset, nextVoice, prev.model),
    }));
  }

  async function runPreview() {
    setTtsTest({ kind: "testing" });
    try {
      const result = await gateway.testTtsConfiguration({
        profile_id: props.profile?.id,
        reuse_asr_profile_id: effectiveReuseAsr ? matchingAsr?.id : undefined,
        tts: toTtsInput(draft, effectiveReuseAsr),
        text: previewText.trim() || "你好，这是语音合成测试。",
      });
      const blob = base64ToWavBlob(result.audio_wav_base64);
      const nextUrl = URL.createObjectURL(blob);
      if (objectUrlRef.current) {
        URL.revokeObjectURL(objectUrlRef.current);
      }
      objectUrlRef.current = nextUrl;
      setTtsTest({
        kind: "ok",
        audio_url: nextUrl,
        elapsed_ms: result.elapsed_ms,
        duration_ms: result.duration_ms,
      });
      if (playRafRef.current !== null) {
        cancelAnimationFrame(playRafRef.current);
      }
      playRafRef.current = requestAnimationFrame(() => {
        playRafRef.current = null;
        void Promise.resolve(audioRef.current?.play()).catch(() => undefined);
      });
    } catch (error: unknown) {
      setTtsTest({ kind: "error", message: errorText(error, "语音合成试听失败。") });
    }
  }

  async function handleSave() {
    const ok = await gateway.saveTtsProfile({
      expected_revision: gateway.speech_config?.revision ?? undefined,
      id: props.profile?.id,
      name: draft.name.trim(),
      provider: draft.provider,
      endpoint: draft.endpoint.trim(),
      model: draft.model.trim(),
      voice: draft.voice.trim(),
      timeout_ms: parseTimeoutMs(draft.timeout_ms),
      credential: !effectiveReuseAsr && draft.credential.trim() ? draft.credential.trim() : undefined,
      clear_credential: false,
      reuse_asr_profile_id: effectiveReuseAsr ? matchingAsr?.id : undefined,
      activate: props.is_active ? true : activate,
    });
    if (ok) {
      props.on_close();
    }
  }

  return (
    <div className={styles.editor_panel}>
      <div className={styles.editor_header}>
        <strong>{props.profile ? "编辑语音合成配置" : "新增语音合成配置"}</strong>
      </div>

      <div className={styles.field_grid}>
        <label className={`${styles.field} ${styles.full_width}`}>
          <span>配置名称</span>
          <input
            aria-label="TTS 配置名称"
            disabled={busy}
            maxLength={64}
            onChange={(event) => {
              setNameTouched(true);
              setDraft({ ...draft, name: event.currentTarget.value });
            }}
            placeholder={defaultTtsProfileName(draft.preset, draft.voice, draft.model)}
            value={draft.name}
          />
        </label>

        <div className={styles.field}>
          <span>服务提供商</span>
          <SelectionPopover<TtsPresetId>
            aria_label="TTS 服务提供商"
            content_width="trigger"
            disabled={busy || testBusy}
            on_open_change={setProviderOpen}
            on_select={handlePresetChange}
            open={providerOpen}
            options={providerOptions}
            selected={draft.preset}
            trigger_content={
              <span className={styles.provider_trigger}>
                <ProviderIcon name={meta.icon} size={16} />
                <span>{meta.label}</span>
              </span>
            }
            trigger_variant="field"
          />
        </div>

        <div className={styles.field}>
          <span>TTS 音色</span>
          <SelectionPopover<string>
            aria_label="TTS 预置音色"
            content_width="trigger"
            disabled={busy || testBusy}
            on_open_change={setVoiceOpen}
            on_select={handleVoiceChange}
            open={voiceOpen}
            options={voiceOptions}
            selected={voiceSelected}
            trigger_variant="field"
          />
        </div>

        {voiceSelected === CUSTOM_VOICE_SENTINEL ? (
          <label className={`${styles.field} ${styles.full_width}`}>
            <span>自定义音色 ID</span>
            <input
              aria-label="TTS 音色名称"
              disabled={busy}
              onChange={(event) => {
                const nextVoice = event.currentTarget.value;
                setDraft((prev) => ({
                  ...prev,
                  voice: nextVoice,
                  name: nameTouched ? prev.name : defaultTtsProfileName(prev.preset, nextVoice, prev.model),
                }));
              }}
              placeholder={defaultVoiceForModel(draft.preset, draft.model)}
              value={draft.voice}
            />
          </label>
        ) : null}

        {matchingAsr ? (
          <div className={`${styles.reuse_box} ${styles.full_width}`}>
            <label className={styles.checkbox_label}>
              <input
                aria-label="复用语音识别的 API Key"
                checked={draft.reuse_asr}
                disabled={busy}
                onChange={(event) => setDraft({ ...draft, reuse_asr: event.currentTarget.checked })}
                type="checkbox"
              />
              复用「{matchingAsr.name}」的 API Key
            </label>
          </div>
        ) : null}

        {!effectiveReuseAsr ? (
          <div className={`${styles.field} ${styles.full_width}`}>
            <div className={styles.credential_row}>
              <label htmlFor={secretId}>API Key</label>
              {props.profile?.credential_configured ? (
                <span className={styles.credential_hint}>
                  已保存（{props.profile.credential_masked ?? "****"}）
                </span>
              ) : null}
            </div>
            <span className={styles.secret_field}>
              <input
                id={secretId}
                aria-label="TTS API Key"
                autoComplete="off"
                disabled={busy}
                onChange={(event) => setDraft({ ...draft, credential: event.currentTarget.value })}
                placeholder={props.profile?.credential_configured ? "留空保持现有密钥" : "请输入 API Key"}
                type={showSecret ? "text" : "password"}
                value={draft.credential}
              />
              <InlineIconButton
                aria-controls={secretId}
                aria-pressed={showSecret}
                className={styles.secret_toggle}
                disabled={busy}
                icon={showSecret ? "eye-off" : "eye"}
                label={showSecret ? "隐藏 TTS 凭据" : "显示 TTS 凭据"}
                onClick={() => setShowSecret(!showSecret)}
                title={showSecret ? "隐藏" : "显示"}
              />
            </span>
          </div>
        ) : null}
      </div>

      <div className={styles.test_panel}>
        <div className={styles.test_header}>
          <span>音色试听</span>
        </div>
        <div className={styles.tts_test_row}>
          <input
            aria-label="TTS 试听文本"
            disabled={busy || testBusy}
            maxLength={120}
            onChange={(event) => setPreviewText(event.currentTarget.value)}
            placeholder="输入试听句子..."
            value={previewText}
          />
          <div className={styles.test_actions}>
            <button
              className={styles.test_primary_button}
              disabled={busy || testBusy}
              onClick={() => void runPreview()}
              type="button"
            >
              {testBusy ? "合成中…" : "试听音色"}
            </button>
          </div>
        </div>
        {ttsTest.kind === "ok" ? (
          <div className={styles.test_result} role="status">
            <span>合成成功 · {ttsTest.elapsed_ms} ms · {(ttsTest.duration_ms / 1000).toFixed(1)} s</span>
            <audio controls ref={audioRef} src={ttsTest.audio_url} />
          </div>
        ) : null}
        {ttsTest.kind === "error" ? (
          <p className={styles.test_error} role="alert">{ttsTest.message}</p>
        ) : null}
      </div>

      <CollapsibleSection
        defaultOpen={Boolean(meta.requires_custom_endpoint)}
        key={meta.requires_custom_endpoint ? "tts-custom" : "tts-preset"}
        title="高级设置"
      >
        <div className={styles.field_grid}>
          <label className={`${styles.field} ${styles.full_width}`}>
            <span>接口地址（Endpoint）</span>
            <input
              aria-label="TTS 接口地址"
              disabled={busy}
              onChange={(event) => setDraft({ ...draft, endpoint: event.currentTarget.value })}
              placeholder={meta.endpoint || "https://.../v1"}
              value={draft.endpoint}
            />
          </label>

          <div className={styles.field}>
            <span>模型预设</span>
            <SelectionPopover<string>
              aria_label="TTS 模型预设"
              content_width="trigger"
              disabled={busy}
              on_open_change={setModelOpen}
              on_select={handleModelChange}
              open={modelOpen}
              options={modelOptions}
              selected={modelSelected}
              trigger_variant="field"
            />
          </div>

          <label className={styles.field}>
            <span>超时（毫秒）</span>
            <input
              aria-label="TTS 超时"
              disabled={busy}
              inputMode="numeric"
              onChange={(event) => setDraft({ ...draft, timeout_ms: event.currentTarget.value.replace(/\D/g, "") })}
              value={draft.timeout_ms}
            />
          </label>

          <label className={`${styles.field} ${styles.full_width}`}>
            <span>模型名称</span>
            <input
              aria-label="TTS 模型名称"
              disabled={busy}
              onChange={(event) => setDraft({ ...draft, model: event.currentTarget.value })}
              placeholder={meta.default_model}
              value={draft.model}
            />
          </label>
        </div>
      </CollapsibleSection>

      <div className={styles.editor_footer}>
        {!props.is_active ? (
          <label className={styles.checkbox_label}>
            <input
              checked={activate}
              disabled={busy}
              onChange={(event) => setActivate(event.currentTarget.checked)}
              type="checkbox"
            />
            保存后设为当前使用
          </label>
        ) : <span />}
        <div className={styles.editor_footer_actions}>
          <button
            className={styles.editor_btn}
            disabled={busy}
            onClick={props.on_close}
            type="button"
          >
            取消
          </button>
          <button
            className={styles.editor_btn_primary}
            disabled={busy}
            onClick={() => void handleSave()}
            type="button"
          >
            {gateway.pending_action === "speech:tts:save" ? "保存中…" : "保存配置"}
          </button>
        </div>
      </div>
    </div>
  );
});
