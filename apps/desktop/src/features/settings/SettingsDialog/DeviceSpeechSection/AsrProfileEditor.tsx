import { observer } from "mobx-react-lite";
import { useEffect, useId, useRef, useState, type ChangeEvent } from "react";
import type {
  AsrProfileSnapshot,
  SpeechProviderKind,
  TtsProfileSnapshot,
  UpdateAsrConfigurationInput,
} from "@ez-assistant/protocol";
import { CollapsibleSection } from "../../../../components/CollapsibleSection";
import { InlineIconButton } from "../../../../components/InlineIconButton";
import { ProviderIcon } from "../../../../components/ProviderIcon";
import { SelectionPopover, type SelectionOption } from "../../../../components/SelectionPopover";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { BrowserWavRecorder, bytesToBase64 } from "../DeviceConsole/wavRecorder";
import {
  ASR_PRESETS,
  CUSTOM_MODEL_SENTINEL,
  detectAsrPreset,
  type AsrPresetId,
} from "./presets";
import styles from "./index.module.scss";

type AsrDraft = {
  name: string;
  preset: AsrPresetId;
  provider: SpeechProviderKind;
  endpoint: string;
  model: string;
  timeout_ms: string;
  credential: string;
};

type AsrTestState =
  | Readonly<{ kind: "idle" }>
  | Readonly<{ kind: "recording" }>
  | Readonly<{ kind: "testing"; source: string }>
  | Readonly<{ kind: "ok"; transcript: string; elapsed_ms: number; source: string }>
  | Readonly<{ kind: "error"; message: string }>;

function shortLabel(label: string): string {
  return label.split(" (")[0] ?? label;
}

export function defaultAsrProfileName(preset: AsrPresetId, model: string): string {
  const base = shortLabel(ASR_PRESETS[preset].label);
  const trimmed = model.trim();
  return trimmed ? `${base} · ${trimmed}` : base;
}

function parseTimeoutMs(raw: string): number {
  const parsed = Number(raw);
  if (!Number.isFinite(parsed) || parsed < 1) return 30000;
  return Math.min(120000, Math.round(parsed));
}

function errorText(error: unknown, fallback: string): string {
  return error instanceof Error && error.message.trim() ? error.message : fallback;
}

function initialDraft(profile: AsrProfileSnapshot | null): AsrDraft {
  if (profile) {
    const preset = detectAsrPreset(profile.provider, profile.endpoint);
    const meta = ASR_PRESETS[preset];
    return {
      name: profile.name,
      preset,
      provider: meta.provider,
      endpoint: profile.endpoint || meta.endpoint,
      model: profile.model || meta.default_model,
      timeout_ms: String(profile.timeout_ms),
      credential: "",
    };
  }
  const meta = ASR_PRESETS.dashscope;
  return {
    name: defaultAsrProfileName("dashscope", meta.default_model),
    preset: "dashscope",
    provider: meta.provider,
    endpoint: meta.endpoint,
    model: meta.default_model,
    timeout_ms: "30000",
    credential: "",
  };
}

function toAsrInput(draft: AsrDraft): UpdateAsrConfigurationInput {
  return {
    enabled: true,
    provider: draft.provider,
    endpoint: draft.endpoint.trim(),
    model: draft.model.trim(),
    timeout_ms: parseTimeoutMs(draft.timeout_ms),
    credential: draft.credential.trim() ? draft.credential.trim() : undefined,
    clear_credential: false,
  };
}

export const AsrProfileEditor = observer(function AsrProfileEditor(props: Readonly<{
  profile: AsrProfileSnapshot | null;
  is_active: boolean;
  default_activate: boolean;
  loopback_tts: TtsProfileSnapshot | null;
  on_close: () => void;
}>) {
  const gateway = useRootStore().device_gateway;
  const secretId = useId();
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const recorderRef = useRef<BrowserWavRecorder | null>(null);

  const [draft, setDraft] = useState<AsrDraft>(() => initialDraft(props.profile));
  const [nameTouched, setNameTouched] = useState<boolean>(Boolean(props.profile));
  const [activate, setActivate] = useState<boolean>(props.is_active || props.default_activate);
  const [providerOpen, setProviderOpen] = useState(false);
  const [modelOpen, setModelOpen] = useState(false);
  const [showSecret, setShowSecret] = useState(false);
  const [asrTest, setAsrTest] = useState<AsrTestState>({ kind: "idle" });

  useEffect(() => () => {
    recorderRef.current?.dispose();
  }, []);

  const busy = gateway.pending_action !== null || gateway.speech_loading;
  const testBusy = asrTest.kind === "testing";
  const meta = ASR_PRESETS[draft.preset];

  const providerOptions: ReadonlyArray<SelectionOption<AsrPresetId>> = Object.values(ASR_PRESETS).map((item) => ({
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

  function handlePresetChange(nextPreset: AsrPresetId) {
    setProviderOpen(false);
    const nextMeta = ASR_PRESETS[nextPreset];
    setDraft((prev) => ({
      ...prev,
      preset: nextPreset,
      provider: nextMeta.provider,
      endpoint: nextMeta.endpoint,
      model: nextMeta.default_model,
      name: nameTouched ? prev.name : defaultAsrProfileName(nextPreset, nextMeta.default_model),
    }));
    setAsrTest({ kind: "idle" });
  }

  function handleModelChange(nextModel: string) {
    setDraft((prev) => ({
      ...prev,
      model: nextModel,
      name: nameTouched ? prev.name : defaultAsrProfileName(prev.preset, nextModel),
    }));
  }

  async function runAsrTestWithWav(wavBase64: string, source: string) {
    setAsrTest({ kind: "testing", source });
    try {
      const result = await gateway.testAsrConfiguration({
        profile_id: props.profile?.id,
        asr: toAsrInput(draft),
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

  async function runLoopbackTest() {
    const tts = props.loopback_tts;
    if (!tts) return;
    setAsrTest({ kind: "testing", source: "TTS 闭环" });
    try {
      const synth = await gateway.testTtsConfiguration({
        profile_id: tts.id,
        tts: {
          enabled: true,
          provider: tts.provider,
          endpoint: tts.endpoint,
          model: tts.model,
          voice: tts.voice,
          timeout_ms: tts.timeout_ms,
          clear_credential: false,
          reuse_asr_credential: false,
        },
        text: "你好，这是语音识别闭环测试。",
      });
      await runAsrTestWithWav(synth.audio_wav_base64, "TTS 闭环");
    } catch (error: unknown) {
      setAsrTest({ kind: "error", message: errorText(error, "TTS 闭环测试失败。") });
    }
  }

  async function handleSave() {
    const ok = await gateway.saveAsrProfile({
      expected_revision: gateway.speech_config?.revision ?? undefined,
      id: props.profile?.id,
      name: draft.name.trim(),
      provider: draft.provider,
      endpoint: draft.endpoint.trim(),
      model: draft.model.trim(),
      timeout_ms: parseTimeoutMs(draft.timeout_ms),
      credential: draft.credential.trim() ? draft.credential.trim() : undefined,
      clear_credential: false,
      activate: props.is_active ? true : activate,
    });
    if (ok) {
      props.on_close();
    }
  }

  const micLabel = asrTest.kind === "recording"
    ? "停止并识别"
    : testBusy
      ? "识别中…"
      : "麦克风测试";

  return (
    <div className={styles.editor_panel}>
      <div className={styles.editor_header}>
        <strong>{props.profile ? "编辑语音识别配置" : "新增语音识别配置"}</strong>
      </div>

      <div className={styles.field_grid}>
        <label className={styles.field}>
          <span>配置名称</span>
          <input
            aria-label="ASR 配置名称"
            disabled={busy}
            maxLength={64}
            onChange={(event) => {
              setNameTouched(true);
              setDraft({ ...draft, name: event.currentTarget.value });
            }}
            placeholder={defaultAsrProfileName(draft.preset, draft.model)}
            value={draft.name}
          />
        </label>

        <div className={styles.field}>
          <span>服务提供商</span>
          <SelectionPopover<AsrPresetId>
            aria_label="ASR 服务提供商"
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
              aria-label="ASR API Key"
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
              label={showSecret ? "隐藏 ASR 凭据" : "显示 ASR 凭据"}
              onClick={() => setShowSecret(!showSecret)}
              title={showSecret ? "隐藏" : "显示"}
            />
          </span>
        </div>
      </div>

      <div className={styles.test_panel}>
        <div className={styles.test_header}>
          <span>识别测试</span>
          <div className={styles.test_actions}>
            <button
              className={styles.test_primary_button}
              disabled={busy || testBusy}
              onClick={() => void toggleMicTest()}
              type="button"
            >
              {micLabel}
            </button>
            <button
              disabled={busy || testBusy || asrTest.kind === "recording"}
              onClick={() => fileInputRef.current?.click()}
              type="button"
            >
              上传 WAV
            </button>
            {props.loopback_tts ? (
              <button
                disabled={busy || testBusy || asrTest.kind === "recording"}
                onClick={() => void runLoopbackTest()}
                type="button"
              >
                TTS 闭环测试
              </button>
            ) : null}
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
          <p className={styles.recording_hint}>正在录音，说完后点击“停止并识别”。</p>
        ) : null}
        {asrTest.kind === "ok" ? (
          <div className={styles.test_result} role="status">
            <strong>「{asrTest.transcript}」</strong>
            <span>{asrTest.source} · {asrTest.elapsed_ms} ms</span>
          </div>
        ) : null}
        {asrTest.kind === "error" ? (
          <p className={styles.test_error} role="alert">{asrTest.message}</p>
        ) : null}
      </div>

      <CollapsibleSection
        defaultOpen={Boolean(meta.requires_custom_endpoint)}
        key={meta.requires_custom_endpoint ? "asr-custom" : "asr-preset"}
        title="高级设置"
      >
        <div className={styles.field_grid}>
          <label className={`${styles.field} ${styles.full_width}`}>
            <span>接口地址（Endpoint）</span>
            <input
              aria-label="ASR 接口地址"
              disabled={busy}
              onChange={(event) => setDraft({ ...draft, endpoint: event.currentTarget.value })}
              placeholder={meta.endpoint || "https://.../v1"}
              value={draft.endpoint}
            />
          </label>

          <div className={styles.field}>
            <span>模型预设</span>
            <SelectionPopover<string>
              aria_label="ASR 模型预设"
              content_width="trigger"
              disabled={busy}
              on_open_change={setModelOpen}
              on_select={(nextModel) => {
                setModelOpen(false);
                if (nextModel !== CUSTOM_MODEL_SENTINEL) {
                  handleModelChange(nextModel);
                }
              }}
              open={modelOpen}
              options={modelOptions}
              selected={modelSelected}
              trigger_variant="field"
            />
          </div>

          <label className={styles.field}>
            <span>超时（毫秒）</span>
            <input
              aria-label="ASR 超时"
              disabled={busy}
              inputMode="numeric"
              onChange={(event) => setDraft({ ...draft, timeout_ms: event.currentTarget.value.replace(/\D/g, "") })}
              value={draft.timeout_ms}
            />
          </label>

          <label className={`${styles.field} ${styles.full_width}`}>
            <span>模型名称</span>
            <input
              aria-label="ASR 模型名称"
              disabled={busy}
              onChange={(event) => handleModelChange(event.currentTarget.value)}
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
            {gateway.pending_action === "speech:asr:save" ? "保存中…" : "保存配置"}
          </button>
        </div>
      </div>
    </div>
  );
});
