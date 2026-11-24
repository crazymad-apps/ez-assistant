import { observer } from "mobx-react-lite";
import { useEffect, useId, useRef, useState, type ChangeEvent } from "react";
import type {
  SpeechConfigurationSnapshot,
  SpeechProviderKind,
  SpeechServiceStatusSnapshot,
  UpdateAsrConfigurationInput,
  UpdateTtsConfigurationInput,
} from "@ez-assistant/protocol";
import { CollapsibleSection } from "../../../../components/CollapsibleSection";
import { InlineIconButton } from "../../../../components/InlineIconButton";
import { SelectionPopover, type SelectionOption } from "../../../../components/SelectionPopover";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { base64ToWavBlob, BrowserWavRecorder, bytesToBase64 } from "../DeviceConsole/wavRecorder";
import shellStyles from "../index.module.scss";
import {
  ASR_PRESETS,
  ASR_PRESET_OPTIONS,
  CUSTOM_MODEL_SENTINEL,
  CUSTOM_VOICE_SENTINEL,
  TTS_PRESETS,
  TTS_PRESET_OPTIONS,
  defaultVoiceForModel,
  detectAsrPreset,
  detectTtsPreset,
  isSharedSameCloudProvider,
  ttsPresetVoices,
  type AsrPresetId,
  type TtsPresetId,
} from "./presets";
import styles from "./index.module.scss";

type AsrDraft = {
  enabled: boolean;
  preset: AsrPresetId;
  provider: SpeechProviderKind;
  endpoint: string;
  model: string;
  timeout_ms: string;
  credential: string;
  clear_credential: boolean;
};

type TtsDraft = {
  enabled: boolean;
  preset: TtsPresetId;
  provider: SpeechProviderKind;
  endpoint: string;
  model: string;
  voice: string;
  custom_voice: boolean;
  timeout_ms: string;
  credential: string;
  clear_credential: boolean;
  reuse_asr_credential: boolean;
};

type AsrTestState =
  | Readonly<{ kind: "idle" }>
  | Readonly<{ kind: "recording" }>
  | Readonly<{ kind: "testing"; source: string }>
  | Readonly<{ kind: "ok"; transcript: string; elapsed_ms: number; source: string }>
  | Readonly<{ kind: "error"; message: string }>;

type TtsTestState =
  | Readonly<{ kind: "idle" }>
  | Readonly<{ kind: "testing" }>
  | Readonly<{ kind: "ok"; audio_url: string; wav_base64: string; elapsed_ms: number; duration_ms: number }>
  | Readonly<{ kind: "error"; message: string }>;

function speechStatusLabel(status?: SpeechServiceStatusSnapshot): string {
  if (status === "ready") return "可用";
  if (status === "degraded") return "部分可用";
  if (status === "unavailable") return "不可用";
  return "待同步";
}

function toAsrDraft(config: SpeechConfigurationSnapshot): AsrDraft {
  const preset = detectAsrPreset(config.asr.provider, config.asr.endpoint);
  const meta = ASR_PRESETS[preset];
  return {
    enabled: config.asr.enabled,
    preset,
    provider: meta.provider,
    endpoint: config.asr.endpoint || meta.endpoint,
    model: config.asr.model || meta.default_model,
    timeout_ms: String(config.asr.timeout_ms),
    credential: "",
    clear_credential: false,
  };
}

function toTtsDraft(config: SpeechConfigurationSnapshot): TtsDraft {
  const asrPreset = detectAsrPreset(config.asr.provider, config.asr.endpoint);
  const preset = detectTtsPreset(config.tts.provider, config.tts.endpoint);
  const meta = TTS_PRESETS[preset];
  const model = config.tts.model || meta.default_model;
  const voice = config.tts.voice || defaultVoiceForModel(preset, model);
  const knownVoices = ttsPresetVoices(preset, model);
  const custom_voice = Boolean(voice) && !knownVoices.some((option) => option.value === voice);
  return {
    enabled: config.tts.enabled,
    preset,
    provider: meta.provider,
    endpoint: config.tts.endpoint || meta.endpoint,
    model,
    voice,
    custom_voice,
    timeout_ms: String(config.tts.timeout_ms),
    credential: "",
    clear_credential: false,
    reuse_asr_credential: isSharedSameCloudProvider(asrPreset, preset) && !config.tts.credential_configured,
  };
}

function emptyAsrDraft(): AsrDraft {
  const meta = ASR_PRESETS.dashscope;
  return {
    enabled: false,
    preset: "dashscope",
    provider: meta.provider,
    endpoint: meta.endpoint,
    model: meta.default_model,
    timeout_ms: "30000",
    credential: "",
    clear_credential: false,
  };
}

function emptyTtsDraft(): TtsDraft {
  const meta = TTS_PRESETS.dashscope;
  return {
    enabled: false,
    preset: "dashscope",
    provider: meta.provider,
    endpoint: meta.endpoint,
    model: meta.default_model,
    voice: meta.default_voice,
    custom_voice: false,
    timeout_ms: "30000",
    credential: "",
    clear_credential: false,
    reuse_asr_credential: true,
  };
}

function parseTimeoutMs(raw: string): number {
  const parsed = Number(raw);
  if (!Number.isFinite(parsed) || parsed < 1) return 30000;
  return Math.min(120000, Math.round(parsed));
}

function errorText(error: unknown, fallback: string): string {
  return error instanceof Error && error.message.trim() ? error.message : fallback;
}

function toAsrInput(draft: AsrDraft): UpdateAsrConfigurationInput {
  return {
    enabled: draft.enabled,
    provider: draft.provider,
    endpoint: draft.endpoint.trim(),
    model: draft.model.trim(),
    timeout_ms: parseTimeoutMs(draft.timeout_ms),
    credential: draft.credential.trim() ? draft.credential.trim() : undefined,
    clear_credential: draft.clear_credential,
  };
}

function toTtsInput(draft: TtsDraft, reuseAsr: boolean): UpdateTtsConfigurationInput {
  return {
    enabled: draft.enabled,
    provider: draft.provider,
    endpoint: draft.endpoint.trim(),
    model: draft.model.trim(),
    voice: draft.voice.trim(),
    timeout_ms: parseTimeoutMs(draft.timeout_ms),
    credential: !reuseAsr && draft.credential.trim() ? draft.credential.trim() : undefined,
    clear_credential: !reuseAsr && draft.clear_credential,
    reuse_asr_credential: reuseAsr,
  };
}

export const DeviceSpeechSection = observer(function DeviceSpeechSection() {
  const store = useRootStore();
  const gateway = store.device_gateway;
  const config = gateway.speech_config;
  const liveStatus = gateway.snapshot?.speech_services;

  const asrSecretId = useId();
  const ttsSecretId = useId();
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const recorderRef = useRef<BrowserWavRecorder | null>(null);
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const ttsObjectUrlRef = useRef<string | null>(null);
  const playRafRef = useRef<number | null>(null);

  const [asr, setAsr] = useState<AsrDraft>(emptyAsrDraft);
  const [tts, setTts] = useState<TtsDraft>(emptyTtsDraft);
  const [asrProviderOpen, setAsrProviderOpen] = useState(false);
  const [asrModelOpen, setAsrModelOpen] = useState(false);
  const [ttsProviderOpen, setTtsProviderOpen] = useState(false);
  const [ttsModelOpen, setTtsModelOpen] = useState(false);
  const [ttsVoiceOpen, setTtsVoiceOpen] = useState(false);
  const [showAsrSecret, setShowAsrSecret] = useState(false);
  const [showTtsSecret, setShowTtsSecret] = useState(false);
  const [ttsPreviewText, setTtsPreviewText] = useState("你好，这是语音合成测试。");
  const [asrTest, setAsrTest] = useState<AsrTestState>({ kind: "idle" });
  const [ttsTest, setTtsTest] = useState<TtsTestState>({ kind: "idle" });
  const [syncedKey, setSyncedKey] = useState<string | null>(null);

  useEffect(() => {
    if (!config) return;
    const key = [
      config.revision ?? "none",
      config.asr.enabled,
      config.tts.enabled,
      config.asr.provider,
      config.tts.provider,
      config.asr.endpoint,
      config.tts.endpoint,
      config.asr.model,
      config.tts.model,
      config.tts.voice,
    ].join(":");
    if (key !== syncedKey) {
      setAsr(toAsrDraft(config));
      setTts(toTtsDraft(config));
      setSyncedKey(key);
    }
  }, [config, syncedKey]);

  useEffect(() => () => {
    recorderRef.current?.dispose();
    if (playRafRef.current !== null) {
      cancelAnimationFrame(playRafRef.current);
      playRafRef.current = null;
    }
    if (ttsObjectUrlRef.current) {
      URL.revokeObjectURL(ttsObjectUrlRef.current);
      ttsObjectUrlRef.current = null;
    }
  }, []);

  const busy = gateway.pending_action !== null || gateway.speech_loading;
  const asrMeta = ASR_PRESETS[asr.preset];
  const ttsMeta = TTS_PRESETS[tts.preset];
  const sameCloudProvider = isSharedSameCloudProvider(asr.preset, tts.preset);
  const effectiveReuseAsr = sameCloudProvider && tts.reuse_asr_credential;
  const asrTestBusy = asrTest.kind === "testing";
  const ttsTestBusy = ttsTest.kind === "testing";

  const asrModelOptions: ReadonlyArray<SelectionOption<string>> = [
    ...asrMeta.models,
    { value: CUSTOM_MODEL_SENTINEL, label: "自定义模型 ID" },
  ];
  const asrModelSelected = asrMeta.models.some((option) => option.value === asr.model)
    ? asr.model
    : CUSTOM_MODEL_SENTINEL;

  const ttsModelOptions: ReadonlyArray<SelectionOption<string>> = [
    ...ttsMeta.models,
    { value: CUSTOM_MODEL_SENTINEL, label: "自定义模型 ID" },
  ];
  const ttsModelSelected = ttsMeta.models.some((option) => option.value === tts.model)
    ? tts.model
    : CUSTOM_MODEL_SENTINEL;

  const ttsVoices = ttsPresetVoices(tts.preset, tts.model);
  const ttsVoiceOptions: ReadonlyArray<SelectionOption<string>> = [
    ...ttsVoices,
    { value: CUSTOM_VOICE_SENTINEL, label: "自定义音色 ID" },
  ];
  const ttsVoiceSelected = tts.custom_voice || !ttsVoices.some((option) => option.value === tts.voice)
    ? CUSTOM_VOICE_SENTINEL
    : tts.voice;

  function handleAsrPresetChange(nextPreset: AsrPresetId) {
    setAsrProviderOpen(false);
    const nextMeta = ASR_PRESETS[nextPreset];
    setAsr((prev) => ({
      ...prev,
      preset: nextPreset,
      provider: nextMeta.provider,
      endpoint: nextMeta.endpoint,
      model: nextMeta.default_model,
      clear_credential: false,
    }));
    setAsrTest({ kind: "idle" });
  }

  function handleTtsPresetChange(nextPreset: TtsPresetId) {
    setTtsProviderOpen(false);
    const nextMeta = TTS_PRESETS[nextPreset];
    const nextReuse = isSharedSameCloudProvider(asr.preset, nextPreset);
    setTts((prev) => ({
      ...prev,
      preset: nextPreset,
      provider: nextMeta.provider,
      endpoint: nextMeta.endpoint,
      model: nextMeta.default_model,
      voice: nextMeta.default_voice,
      custom_voice: false,
      clear_credential: false,
      reuse_asr_credential: nextReuse ? prev.reuse_asr_credential : false,
    }));
    setTtsTest({ kind: "idle" });
  }

  function handleTtsModelChange(nextModel: string) {
    setTtsModelOpen(false);
    if (nextModel === CUSTOM_MODEL_SENTINEL) return;
    setTts((prev) => {
      const nextVoices = ttsPresetVoices(prev.preset, nextModel);
      const stillValid = !prev.custom_voice && nextVoices.some((option) => option.value === prev.voice);
      const nextVoice = stillValid ? prev.voice : defaultVoiceForModel(prev.preset, nextModel);
      return {
        ...prev,
        model: nextModel,
        voice: nextVoice,
        custom_voice: false,
      };
    });
  }

  function handleTtsVoiceChange(nextVoice: string) {
    setTtsVoiceOpen(false);
    if (nextVoice === CUSTOM_VOICE_SENTINEL) {
      setTts((prev) => ({ ...prev, custom_voice: true }));
      return;
    }
    setTts((prev) => ({ ...prev, voice: nextVoice, custom_voice: false }));
  }

  async function saveConfiguration() {
    await gateway.updateSpeechConfiguration({
      expected_revision: config?.revision ?? undefined,
      asr: toAsrInput(asr),
      tts: toTtsInput(tts, effectiveReuseAsr),
    });
  }

  async function runAsrTestWithWav(wavBase64: string, source: string) {
    setAsrTest({ kind: "testing", source });
    try {
      const result = await gateway.testAsrConfiguration({
        asr: toAsrInput(asr),
        audio_wav_base64: wavBase64,
      });
      setAsrTest({
        kind: "ok",
        transcript: result.transcript,
        elapsed_ms: result.elapsed_ms,
        source,
      });
    } catch (error: unknown) {
      setAsrTest({ kind: "error", message: errorText(error, "语音识别测试失败。") });
    }
  }

  async function toggleMicTest() {
    if (asrTest.kind === "recording") {
      try {
        const sample = await recorderRef.current?.stop();
        if (!sample) {
          setAsrTest({ kind: "idle" });
          return;
        }
        await runAsrTestWithWav(sample.base64, sample.label);
      } catch (error: unknown) {
        setAsrTest({ kind: "error", message: errorText(error, "无法结束麦克风录音。") });
      }
      return;
    }
    try {
      const recorder = recorderRef.current ?? new BrowserWavRecorder();
      recorderRef.current = recorder;
      await recorder.start();
      setAsrTest({ kind: "recording" });
    } catch (error: unknown) {
      setAsrTest({ kind: "error", message: errorText(error, "无法启动麦克风录音。") });
    }
  }

  async function handleWavFileSelected(event: ChangeEvent<HTMLInputElement>) {
    const file = event.currentTarget.files?.[0];
    event.currentTarget.value = "";
    if (!file) return;
    try {
      const buffer = await file.arrayBuffer();
      const base64 = bytesToBase64(new Uint8Array(buffer));
      await runAsrTestWithWav(base64, file.name);
    } catch (error: unknown) {
      setAsrTest({ kind: "error", message: errorText(error, "读取测试音频文件失败。") });
    }
  }

  async function runTtsPreview(): Promise<string | null> {
    setTtsTest({ kind: "testing" });
    try {
      const result = await gateway.testTtsConfiguration({
        tts: toTtsInput(tts, effectiveReuseAsr),
        fallback_asr_credential: effectiveReuseAsr && asr.credential.trim() ? asr.credential.trim() : undefined,
        text: ttsPreviewText.trim() || "你好，这是语音合成测试。",
      });
      const blob = base64ToWavBlob(result.audio_wav_base64);
      const nextUrl = URL.createObjectURL(blob);
      if (ttsObjectUrlRef.current) {
        URL.revokeObjectURL(ttsObjectUrlRef.current);
      }
      ttsObjectUrlRef.current = nextUrl;
      setTtsTest({
        kind: "ok",
        audio_url: nextUrl,
        wav_base64: result.audio_wav_base64,
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
      return result.audio_wav_base64;
    } catch (error: unknown) {
      setTtsTest({ kind: "error", message: errorText(error, "语音合成试听失败。") });
      return null;
    }
  }

  async function runLoopbackAsrTest() {
    if (ttsTest.kind === "ok") {
      await runAsrTestWithWav(ttsTest.wav_base64, "TTS 合成音频闭环");
      return;
    }
    const synthesized = await runTtsPreview();
    if (synthesized) {
      await runAsrTestWithWav(synthesized, "TTS 合成音频闭环");
    }
  }

  const asrCredentialHint = config?.asr.credential_configured
    ? `已配置凭据 ${config.asr.credential_masked}`
    : "尚未配置凭据";
  const ttsCredentialHint = config?.tts.credential_configured
    ? `已配置凭据 ${config.tts.credential_masked}`
    : "尚未配置凭据";

  let asrMicLabel = "录音测试识别";
  if (asrTest.kind === "recording") asrMicLabel = "停止并识别";
  if (asrTest.kind === "testing") asrMicLabel = "识别测试中…";

  return (
    <section className={shellStyles.device_section}>
      <header>
        <h4>语音服务</h4>
        <button
          disabled={busy}
          onClick={() => void gateway.loadSpeechConfiguration()}
          type="button"
        >
          {gateway.speech_loading ? "读取中…" : "重新读取"}
        </button>
      </header>

      <div className={styles.speech_cards}>
        <article className={styles.speech_card}>
          <div className={styles.speech_card_header}>
            <div>
              <strong>语音识别</strong>
              <span className={styles.status_badge} data-status={liveStatus?.asr}>
                {speechStatusLabel(liveStatus?.asr)}
              </span>
            </div>
            <label className={styles.switch}>
              <input
                aria-label="启用语音识别"
                checked={asr.enabled}
                disabled={busy}
                onChange={(event) => setAsr({ ...asr, enabled: event.currentTarget.checked })}
                type="checkbox"
              />
              <span />
              <b>{asr.enabled ? "已启用" : "已停用"}</b>
            </label>
          </div>

          <div className={styles.field_grid}>
            <div className={`${styles.field} ${styles.full_width}`}>
              <span>服务提供商</span>
              <SelectionPopover<AsrPresetId>
                aria_label="ASR 服务提供商"
                content_width="trigger"
                disabled={busy || asrTestBusy}
                on_open_change={setAsrProviderOpen}
                on_select={handleAsrPresetChange}
                open={asrProviderOpen}
                options={ASR_PRESET_OPTIONS}
                selected={asr.preset}
                trigger_variant="field"
              />
              <p className={styles.preset_hint}>{asrMeta.hint}</p>
            </div>

            <div className={`${styles.field} ${styles.full_width}`}>
              <div className={styles.credential_row}>
                <label htmlFor={asrSecretId}>API Key 凭据</label>
                <span className={styles.credential_hint}>{asrCredentialHint}</span>
              </div>
              <span className={styles.secret_field}>
                <input
                  id={asrSecretId}
                  aria-label="ASR API Key"
                  autoComplete="off"
                  disabled={busy || asr.clear_credential}
                  onChange={(event) => setAsr({ ...asr, credential: event.currentTarget.value })}
                  placeholder={config?.asr.credential_configured ? "已保存，留空保留现有" : "请输入 API Key"}
                  type={showAsrSecret ? "text" : "password"}
                  value={asr.credential}
                />
                <InlineIconButton
                  aria-controls={asrSecretId}
                  aria-pressed={showAsrSecret}
                  className={styles.secret_toggle}
                  disabled={busy || asr.clear_credential}
                  icon={showAsrSecret ? "eye-off" : "eye"}
                  label={showAsrSecret ? "隐藏 ASR 凭据" : "显示 ASR 凭据"}
                  onClick={() => setShowAsrSecret(!showAsrSecret)}
                  title={showAsrSecret ? "隐藏" : "显示"}
                />
              </span>
              {config?.asr.credential_configured ? (
                <label className={styles.clear_toggle}>
                  <input
                    checked={asr.clear_credential}
                    disabled={busy}
                    onChange={(event) => {
                      const clear_credential = event.currentTarget.checked;
                      setAsr({ ...asr, clear_credential, credential: clear_credential ? "" : asr.credential });
                    }}
                    type="checkbox"
                  />
                  清除已保存的 ASR 凭据
                </label>
              ) : null}
            </div>
          </div>

          <div className={styles.test_panel}>
            <div className={styles.test_header}>
              <span>识别测试（可直接验证当前填写的配置）</span>
              <div className={styles.test_actions}>
                <button
                  className={styles.test_primary_button}
                  disabled={busy || asrTestBusy}
                  onClick={() => void toggleMicTest()}
                  type="button"
                >
                  {asrMicLabel}
                </button>
                <button
                  disabled={busy || asrTestBusy || asrTest.kind === "recording"}
                  onClick={() => fileInputRef.current?.click()}
                  type="button"
                >
                  上传 WAV 测试
                </button>
                <button
                  disabled={busy || asrTestBusy || ttsTestBusy || asrTest.kind === "recording"}
                  onClick={() => void runLoopbackAsrTest()}
                  type="button"
                >
                  用 TTS 音频闭环测试
                </button>
                <input
                  accept=".wav,audio/wav"
                  aria-label="上传 ASR 测试 WAV 文件"
                  className={styles.hidden_file_input}
                  onChange={(event) => void handleWavFileSelected(event)}
                  ref={fileInputRef}
                  type="file"
                />
              </div>
            </div>
            {asrTest.kind === "recording" ? (
              <p className={styles.preset_hint}>正在录音，请说一句话，然后点击“停止并识别”。</p>
            ) : null}
            {asrTest.kind === "ok" ? (
              <div className={styles.test_result} role="status">
                <strong>识别结果：「{asrTest.transcript}」</strong>
                <span>来源：{asrTest.source} · 耗时 {asrTest.elapsed_ms} ms</span>
              </div>
            ) : null}
            {asrTest.kind === "error" ? (
              <p className={styles.test_error} role="alert">{asrTest.message}</p>
            ) : null}
          </div>

          <CollapsibleSection
            defaultOpen={Boolean(asrMeta.requires_custom_endpoint)}
            key={asrMeta.requires_custom_endpoint ? "asr-custom" : "asr-preset"}
            title="高级设置"
          >
            <div className={styles.field_grid}>
              <label className={`${styles.field} ${styles.full_width}`}>
                <span>接口地址（Endpoint）</span>
                <input
                  aria-label="ASR 接口地址"
                  disabled={busy}
                  onChange={(event) => setAsr({ ...asr, endpoint: event.currentTarget.value })}
                  placeholder={asrMeta.endpoint || "https://.../v1"}
                  value={asr.endpoint}
                />
              </label>

              <div className={styles.field}>
                <span>模型预设</span>
                <SelectionPopover<string>
                  aria_label="ASR 模型预设"
                  content_width="trigger"
                  disabled={busy}
                  on_open_change={setAsrModelOpen}
                  on_select={(nextModel) => {
                    setAsrModelOpen(false);
                    if (nextModel !== CUSTOM_MODEL_SENTINEL) {
                      setAsr({ ...asr, model: nextModel });
                    }
                  }}
                  open={asrModelOpen}
                  options={asrModelOptions}
                  selected={asrModelSelected}
                  trigger_variant="field"
                />
              </div>

              <label className={styles.field}>
                <span>超时（毫秒）</span>
                <input
                  aria-label="ASR 超时"
                  disabled={busy}
                  inputMode="numeric"
                  onChange={(event) => setAsr({ ...asr, timeout_ms: event.currentTarget.value.replace(/\D/g, "") })}
                  value={asr.timeout_ms}
                />
              </label>

              <label className={`${styles.field} ${styles.full_width}`}>
                <span>模型名称</span>
                <input
                  aria-label="ASR 模型名称"
                  disabled={busy}
                  onChange={(event) => setAsr({ ...asr, model: event.currentTarget.value })}
                  placeholder={asrMeta.default_model}
                  value={asr.model}
                />
              </label>
            </div>
          </CollapsibleSection>
        </article>

        <article className={styles.speech_card}>
          <div className={styles.speech_card_header}>
            <div>
              <strong>语音播放</strong>
              <span className={styles.status_badge} data-status={liveStatus?.tts}>
                {speechStatusLabel(liveStatus?.tts)}
              </span>
            </div>
            <label className={styles.switch}>
              <input
                aria-label="启用语音合成"
                checked={tts.enabled}
                disabled={busy}
                onChange={(event) => setTts({ ...tts, enabled: event.currentTarget.checked })}
                type="checkbox"
              />
              <span />
              <b>{tts.enabled ? "已启用" : "已停用"}</b>
            </label>
          </div>

          <div className={styles.field_grid}>
            <div className={styles.field}>
              <span>服务提供商</span>
              <SelectionPopover<TtsPresetId>
                aria_label="TTS 服务提供商"
                content_width="trigger"
                disabled={busy || ttsTestBusy}
                on_open_change={setTtsProviderOpen}
                on_select={handleTtsPresetChange}
                open={ttsProviderOpen}
                options={TTS_PRESET_OPTIONS}
                selected={tts.preset}
                trigger_variant="field"
              />
            </div>

            <div className={styles.field}>
              <span>TTS 音色</span>
              <SelectionPopover<string>
                aria_label="TTS 预置音色"
                content_width="trigger"
                disabled={busy || ttsTestBusy}
                on_open_change={setTtsVoiceOpen}
                on_select={handleTtsVoiceChange}
                open={ttsVoiceOpen}
                options={ttsVoiceOptions}
                selected={ttsVoiceSelected}
                trigger_variant="field"
              />
            </div>

            <p className={`${styles.preset_hint} ${styles.full_width}`}>{ttsMeta.hint}</p>

            {ttsVoiceSelected === CUSTOM_VOICE_SENTINEL ? (
              <label className={`${styles.field} ${styles.full_width}`}>
                <span>自定义音色 ID</span>
                <input
                  aria-label="TTS 音色名称"
                  disabled={busy}
                  onChange={(event) => setTts({ ...tts, voice: event.currentTarget.value })}
                  placeholder={defaultVoiceForModel(tts.preset, tts.model)}
                  value={tts.voice}
                />
              </label>
            ) : null}

            {sameCloudProvider ? (
              <div className={`${styles.reuse_box} ${styles.full_width}`}>
                <label className={styles.clear_toggle}>
                  <input
                    aria-label="复用语音识别的 API Key"
                    checked={tts.reuse_asr_credential}
                    disabled={busy}
                    onChange={(event) => setTts({ ...tts, reuse_asr_credential: event.currentTarget.checked })}
                    type="checkbox"
                  />
                  复用上方「语音识别」的 API Key（同厂商只需填一次）
                </label>
              </div>
            ) : null}

            {!effectiveReuseAsr ? (
              <div className={`${styles.field} ${styles.full_width}`}>
                <div className={styles.credential_row}>
                  <label htmlFor={ttsSecretId}>API Key 凭据</label>
                  <span className={styles.credential_hint}>{ttsCredentialHint}</span>
                </div>
                <span className={styles.secret_field}>
                  <input
                    id={ttsSecretId}
                    aria-label="TTS API Key"
                    autoComplete="off"
                    disabled={busy || tts.clear_credential}
                    onChange={(event) => setTts({ ...tts, credential: event.currentTarget.value })}
                    placeholder={config?.tts.credential_configured ? "已保存，留空保留现有" : "请输入 API Key"}
                    type={showTtsSecret ? "text" : "password"}
                    value={tts.credential}
                  />
                  <InlineIconButton
                    aria-controls={ttsSecretId}
                    aria-pressed={showTtsSecret}
                    className={styles.secret_toggle}
                    disabled={busy || tts.clear_credential}
                    icon={showTtsSecret ? "eye-off" : "eye"}
                    label={showTtsSecret ? "隐藏 TTS 凭据" : "显示 TTS 凭据"}
                    onClick={() => setShowTtsSecret(!showTtsSecret)}
                    title={showTtsSecret ? "隐藏" : "显示"}
                  />
                </span>
                {config?.tts.credential_configured ? (
                  <label className={styles.clear_toggle}>
                    <input
                      checked={tts.clear_credential}
                      disabled={busy}
                      onChange={(event) => {
                        const clear_credential = event.currentTarget.checked;
                        setTts({ ...tts, clear_credential, credential: clear_credential ? "" : tts.credential });
                      }}
                      type="checkbox"
                    />
                    清除已保存的 TTS 凭据
                  </label>
                ) : null}
              </div>
            ) : null}
          </div>

          <div className={styles.test_panel}>
            <div className={styles.test_header}>
              <span>音色试听（切换音色后点击试听即可直接播放）</span>
            </div>
            <div className={styles.tts_test_row}>
              <input
                aria-label="TTS 试听文本"
                disabled={busy || ttsTestBusy}
                maxLength={120}
                onChange={(event) => setTtsPreviewText(event.currentTarget.value)}
                placeholder="输入试听句子..."
                value={ttsPreviewText}
              />
              <div className={styles.test_actions}>
                <button
                  className={styles.test_primary_button}
                  disabled={busy || ttsTestBusy}
                  onClick={() => void runTtsPreview()}
                  type="button"
                >
                  {ttsTestBusy ? "合成中…" : "试听音色"}
                </button>
              </div>
            </div>
            {ttsTest.kind === "ok" ? (
              <div className={styles.test_result} role="status">
                <span>合成成功 · 耗时 {ttsTest.elapsed_ms} ms · 音频 {(ttsTest.duration_ms / 1000).toFixed(1)} s</span>
                <audio controls ref={audioRef} src={ttsTest.audio_url} />
              </div>
            ) : null}
            {ttsTest.kind === "error" ? (
              <p className={styles.test_error} role="alert">{ttsTest.message}</p>
            ) : null}
          </div>

          <CollapsibleSection
            defaultOpen={Boolean(ttsMeta.requires_custom_endpoint)}
            key={ttsMeta.requires_custom_endpoint ? "tts-custom" : "tts-preset"}
            title="高级设置"
          >
            <div className={styles.field_grid}>
              <label className={`${styles.field} ${styles.full_width}`}>
                <span>接口地址（Endpoint）</span>
                <input
                  aria-label="TTS 接口地址"
                  disabled={busy}
                  onChange={(event) => setTts({ ...tts, endpoint: event.currentTarget.value })}
                  placeholder={ttsMeta.endpoint || "https://.../v1"}
                  value={tts.endpoint}
                />
              </label>

              <div className={styles.field}>
                <span>模型预设</span>
                <SelectionPopover<string>
                  aria_label="TTS 模型预设"
                  content_width="trigger"
                  disabled={busy}
                  on_open_change={setTtsModelOpen}
                  on_select={handleTtsModelChange}
                  open={ttsModelOpen}
                  options={ttsModelOptions}
                  selected={ttsModelSelected}
                  trigger_variant="field"
                />
              </div>

              <label className={styles.field}>
                <span>超时（毫秒）</span>
                <input
                  aria-label="TTS 超时"
                  disabled={busy}
                  inputMode="numeric"
                  onChange={(event) => setTts({ ...tts, timeout_ms: event.currentTarget.value.replace(/\D/g, "") })}
                  value={tts.timeout_ms}
                />
              </label>

              <label className={`${styles.field} ${styles.full_width}`}>
                <span>模型名称</span>
                <input
                  aria-label="TTS 模型名称"
                  disabled={busy}
                  onChange={(event) => setTts({ ...tts, model: event.currentTarget.value })}
                  placeholder={ttsMeta.default_model}
                  value={tts.model}
                />
              </label>
            </div>
          </CollapsibleSection>
        </article>
      </div>

      <footer className={styles.footer_actions}>
        <span>选好服务商与音色并填写 API Key 后即可测试，保存后自动生效。</span>
        <button
          disabled={busy}
          onClick={() => void saveConfiguration()}
          type="button"
        >
          {gateway.pending_action === "speech" ? "保存中…" : "保存并应用"}
        </button>
      </footer>
    </section>
  );
});
