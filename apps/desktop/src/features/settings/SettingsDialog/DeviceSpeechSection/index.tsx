import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState, type ChangeEvent } from "react";
import type {
  AsrProfileSnapshot,
  SpeechCapabilityKind,
  SpeechServiceStatusSnapshot,
  TtsProfileSnapshot,
} from "@ez-assistant/protocol";
import { Icon } from "../../../../components/Icon";
import { PresenceBoundary } from "../../../../components/Presence";
import { ProviderIcon } from "../../../../components/ProviderIcon";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { SessionActionDialog } from "../../../sessions/SessionActionDialog";
import { base64ToWavBlob, BrowserWavRecorder, bytesToBase64 } from "../DeviceConsole/wavRecorder";
import shellStyles from "../index.module.scss";
import { AsrProfileEditor } from "./AsrProfileEditor";
import {
  ASR_PRESETS,
  TTS_PRESETS,
  detectAsrPreset,
  detectTtsPreset,
} from "./presets";
import { TtsProfileEditor } from "./TtsProfileEditor";
import styles from "./index.module.scss";

type AsrEditorState =
  | null
  | Readonly<{ mode: "create" }>
  | Readonly<{ mode: "edit"; profile: AsrProfileSnapshot }>;

type TtsEditorState =
  | null
  | Readonly<{ mode: "create" }>
  | Readonly<{ mode: "edit"; profile: TtsProfileSnapshot }>;

type PendingDelete = Readonly<{
  capability: SpeechCapabilityKind;
  id: string;
  name: string;
}>;

type QuickAsrTestState =
  | Readonly<{ kind: "idle" }>
  | Readonly<{ kind: "recording" }>
  | Readonly<{ kind: "testing"; source: string }>
  | Readonly<{ kind: "ok"; transcript: string; elapsed_ms: number; source: string }>
  | Readonly<{ kind: "error"; message: string }>;

type QuickTtsTestState =
  | Readonly<{ kind: "idle" }>
  | Readonly<{ kind: "testing" }>
  | Readonly<{ kind: "ok"; audio_url: string; elapsed_ms: number; duration_ms: number }>
  | Readonly<{ kind: "error"; message: string }>;

function speechStatusLabel(status?: SpeechServiceStatusSnapshot): string {
  if (status === "ready") return "可用";
  if (status === "degraded") return "部分可用";
  if (status === "unavailable") return "不可用";
  return "待同步";
}

function shortLabel(label: string): string {
  return label.split(" (")[0] ?? label;
}

function errorText(error: unknown, fallback: string): string {
  return error instanceof Error && error.message.trim() ? error.message : fallback;
}

const AsrQuickTestBar = observer(function AsrQuickTestBar(props: Readonly<{
  profile: AsrProfileSnapshot;
  loopback_tts: TtsProfileSnapshot | null;
}>) {
  const gateway = useRootStore().device_gateway;
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const recorderRef = useRef<BrowserWavRecorder | null>(null);
  const [state, setState] = useState<QuickAsrTestState>({ kind: "idle" });

  useEffect(() => () => {
    recorderRef.current?.dispose();
  }, []);

  const busy = gateway.pending_action !== null || state.kind === "testing";

  async function runWithWav(wavBase64: string, source: string) {
    setState({ kind: "testing", source });
    try {
      const result = await gateway.testAsrConfiguration({
        profile_id: props.profile.id,
        asr: {
          enabled: true,
          provider: props.profile.provider,
          endpoint: props.profile.endpoint,
          model: props.profile.model,
          timeout_ms: props.profile.timeout_ms,
          clear_credential: false,
        },
        audio_wav_base64: wavBase64,
      });
      setState({
        kind: "ok",
        transcript: result.transcript,
        elapsed_ms: result.elapsed_ms,
        source,
      });
    } catch (error: unknown) {
      setState({ kind: "error", message: errorText(error, "语音识别测试失败。") });
    }
  }

  async function toggleMic() {
    if (state.kind === "recording") {
      try {
        const sample = await recorderRef.current?.stop();
        if (!sample) {
          setState({ kind: "idle" });
          return;
        }
        await runWithWav(sample.base64, sample.label);
      } catch (error: unknown) {
        setState({ kind: "error", message: errorText(error, "无法结束麦克风录音。") });
      }
      return;
    }
    try {
      const recorder = recorderRef.current ?? new BrowserWavRecorder();
      recorderRef.current = recorder;
      await recorder.start();
      setState({ kind: "recording" });
    } catch (error: unknown) {
      setState({ kind: "error", message: errorText(error, "无法启动麦克风录音。") });
    }
  }

  async function handleFile(event: ChangeEvent<HTMLInputElement>) {
    const file = event.currentTarget.files?.[0];
    event.currentTarget.value = "";
    if (!file) return;
    try {
      const buffer = await file.arrayBuffer();
      await runWithWav(bytesToBase64(new Uint8Array(buffer)), file.name);
    } catch (error: unknown) {
      setState({ kind: "error", message: errorText(error, "读取测试音频文件失败。") });
    }
  }

  async function runLoopback() {
    const tts = props.loopback_tts;
    if (!tts) return;
    setState({ kind: "testing", source: "TTS 闭环" });
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
      await runWithWav(synth.audio_wav_base64, "TTS 闭环");
    } catch (error: unknown) {
      setState({ kind: "error", message: errorText(error, "TTS 闭环测试失败。") });
    }
  }

  return (
    <div className={styles.quick_test_bar}>
      <div className={styles.test_actions}>
        <button
          className={styles.test_primary_button}
          disabled={busy}
          onClick={() => void toggleMic()}
          type="button"
        >
          {state.kind === "recording" ? "停止并识别" : state.kind === "testing" ? "识别中…" : "麦克风测试"}
        </button>
        <button
          disabled={busy || state.kind === "recording"}
          onClick={() => fileInputRef.current?.click()}
          type="button"
        >
          上传 WAV
        </button>
        {props.loopback_tts ? (
          <button
            disabled={busy || state.kind === "recording"}
            onClick={() => void runLoopback()}
            type="button"
          >
            TTS 闭环测试
          </button>
        ) : null}
        <input
          accept=".wav,audio/wav"
          aria-label={`上传 ${props.profile.name} 测试 WAV`}
          className={styles.hidden_file_input}
          onChange={(event) => void handleFile(event)}
          ref={fileInputRef}
          type="file"
        />
      </div>
      {state.kind === "recording" ? (
        <p className={styles.recording_hint}>正在录音，说完后点击“停止并识别”。</p>
      ) : null}
      {state.kind === "ok" ? (
        <div className={styles.test_result} role="status">
          <strong>「{state.transcript}」</strong>
          <span>{state.source} · {state.elapsed_ms} ms</span>
        </div>
      ) : null}
      {state.kind === "error" ? (
        <p className={styles.test_error} role="alert">{state.message}</p>
      ) : null}
    </div>
  );
});

const TtsQuickPreviewBar = observer(function TtsQuickPreviewBar(props: Readonly<{
  profile: TtsProfileSnapshot;
}>) {
  const gateway = useRootStore().device_gateway;
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const objectUrlRef = useRef<string | null>(null);
  const playRafRef = useRef<number | null>(null);
  const [text, setText] = useState("你好，这是语音合成测试。");
  const [state, setState] = useState<QuickTtsTestState>({ kind: "idle" });

  async function runPreview(sentence: string) {
    setState({ kind: "testing" });
    try {
      const result = await gateway.testTtsConfiguration({
        profile_id: props.profile.id,
        tts: {
          enabled: true,
          provider: props.profile.provider,
          endpoint: props.profile.endpoint,
          model: props.profile.model,
          voice: props.profile.voice,
          timeout_ms: props.profile.timeout_ms,
          clear_credential: false,
          reuse_asr_credential: false,
        },
        text: sentence.trim() || "你好，这是语音合成测试。",
      });
      const blob = base64ToWavBlob(result.audio_wav_base64);
      const nextUrl = URL.createObjectURL(blob);
      if (objectUrlRef.current) {
        URL.revokeObjectURL(objectUrlRef.current);
      }
      objectUrlRef.current = nextUrl;
      setState({
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
      setState({ kind: "error", message: errorText(error, "语音合成试听失败。") });
    }
  }

  useEffect(() => {
    void runPreview("你好，这是语音合成测试。");
    return () => {
      if (playRafRef.current !== null) {
        cancelAnimationFrame(playRafRef.current);
        playRafRef.current = null;
      }
      if (objectUrlRef.current) {
        URL.revokeObjectURL(objectUrlRef.current);
        objectUrlRef.current = null;
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.profile.id]);

  const busy = gateway.pending_action !== null || state.kind === "testing";

  return (
    <div className={styles.quick_test_bar}>
      <div className={styles.tts_test_row}>
        <input
          aria-label={`试听 ${props.profile.name} 文本`}
          disabled={busy}
          maxLength={120}
          onChange={(event) => setText(event.currentTarget.value)}
          placeholder="输入试听句子..."
          value={text}
        />
        <div className={styles.test_actions}>
          <button
            className={styles.test_primary_button}
            disabled={busy}
            onClick={() => void runPreview(text)}
            type="button"
          >
            {state.kind === "testing" ? "合成中…" : "试听"}
          </button>
        </div>
      </div>
      {state.kind === "ok" ? (
        <div className={styles.test_result} role="status">
          <span>合成成功 · {state.elapsed_ms} ms · {(state.duration_ms / 1000).toFixed(1)} s</span>
          <audio controls ref={audioRef} src={state.audio_url} />
        </div>
      ) : null}
      {state.kind === "error" ? (
        <p className={styles.test_error} role="alert">{state.message}</p>
      ) : null}
    </div>
  );
});

export const DeviceSpeechSection = observer(function DeviceSpeechSection() {
  const gateway = useRootStore().device_gateway;
  const config = gateway.speech_config;
  const liveStatus = gateway.snapshot?.speech_services;

  const asrProfiles = config?.asr_profiles ?? [];
  const ttsProfiles = config?.tts_profiles ?? [];
  const activeAsrId = config?.active_asr_id ?? null;
  const activeTtsId = config?.active_tts_id ?? null;

  const lastAsrIdRef = useRef<string | null>(null);
  const lastTtsIdRef = useRef<string | null>(null);
  useEffect(() => {
    if (activeAsrId) lastAsrIdRef.current = activeAsrId;
  }, [activeAsrId]);
  useEffect(() => {
    if (activeTtsId) lastTtsIdRef.current = activeTtsId;
  }, [activeTtsId]);

  const [asrEditor, setAsrEditor] = useState<AsrEditorState>(null);
  const [ttsEditor, setTtsEditor] = useState<TtsEditorState>(null);
  const [testingAsrId, setTestingAsrId] = useState<string | null>(null);
  const [testingTtsId, setTestingTtsId] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<PendingDelete | null>(null);

  const busy = gateway.pending_action !== null || gateway.speech_loading;
  const loopbackTts = ttsProfiles.find((item) => item.id === activeTtsId) ?? ttsProfiles[0] ?? null;

  async function toggleAsrMaster(checked: boolean) {
    if (!checked) {
      await gateway.setActiveSpeechProfile("asr", null);
      return;
    }
    const target = asrProfiles.find((item) => item.id === lastAsrIdRef.current)?.id
      ?? asrProfiles[0]?.id;
    if (target) {
      await gateway.setActiveSpeechProfile("asr", target);
    }
  }

  async function toggleTtsMaster(checked: boolean) {
    if (!checked) {
      await gateway.setActiveSpeechProfile("tts", null);
      return;
    }
    const target = ttsProfiles.find((item) => item.id === lastTtsIdRef.current)?.id
      ?? ttsProfiles[0]?.id;
    if (target) {
      await gateway.setActiveSpeechProfile("tts", target);
    }
  }

  async function confirmDelete() {
    if (!pendingDelete) return;
    const { capability, id } = pendingDelete;
    const ok = await gateway.deleteSpeechProfile(capability, id);
    if (ok) {
      if (capability === "asr") {
        if (testingAsrId === id) setTestingAsrId(null);
        if (asrEditor?.mode === "edit" && asrEditor.profile.id === id) setAsrEditor(null);
      } else {
        if (testingTtsId === id) setTestingTtsId(null);
        if (ttsEditor?.mode === "edit" && ttsEditor.profile.id === id) setTtsEditor(null);
      }
      setPendingDelete(null);
    }
  }

  return (
    <section className={shellStyles.device_section}>
      <header>
        <h4>语音服务</h4>
        {!config && gateway.error_message ? (
          <button
            disabled={busy}
            onClick={() => void gateway.loadSpeechConfiguration()}
            type="button"
          >
            {gateway.speech_loading ? "读取中…" : "重试"}
          </button>
        ) : null}
      </header>

      <div className={styles.speech_cards}>
        {/* 语音识别 (ASR) 配置列表 */}
        <article className={styles.speech_card}>
          <div className={styles.speech_card_header}>
            <div className={styles.speech_card_title}>
              <strong>语音识别 (ASR)</strong>
              <span className={styles.status_badge} data-status={liveStatus?.asr}>
                {speechStatusLabel(liveStatus?.asr)}
              </span>
            </div>
            <div className={styles.speech_card_controls}>
              <label
                className={styles.switch}
                data-disabled={busy || asrProfiles.length === 0}
              >
                <input
                  aria-label="启用语音识别"
                  checked={Boolean(activeAsrId)}
                  disabled={busy || asrProfiles.length === 0}
                  onChange={(event) => void toggleAsrMaster(event.currentTarget.checked)}
                  type="checkbox"
                />
                <span />
                <b>{activeAsrId ? "已启用" : "已停用"}</b>
              </label>
              <button
                aria-label="新增语音识别配置"
                className={styles.add_btn}
                disabled={busy}
                onClick={() => {
                  setTestingAsrId(null);
                  setAsrEditor({ mode: "create" });
                }}
                type="button"
              >
                <Icon name="plus" size={13} />
                新增配置
              </button>
            </div>
          </div>

          {asrProfiles.length > 0 ? (
            <div className={styles.profile_list}>
              {asrProfiles.map((profile) => {
                const isActive = profile.id === activeAsrId;
                const presetId = detectAsrPreset(profile.provider, profile.endpoint);
                const meta = ASR_PRESETS[presetId];
                const isTesting = testingAsrId === profile.id;
                const subtitleParts = [
                  shortLabel(meta.label),
                  profile.model,
                  profile.credential_masked,
                ].filter(Boolean);
                return (
                  <div
                    className={styles.profile_item}
                    data-active={isActive}
                    key={profile.id}
                  >
                    <div className={styles.profile_row}>
                      <div className={styles.profile_main}>
                        <span className={styles.profile_dot} data-active={isActive} />
                        <ProviderIcon name={meta.icon} size={18} />
                        <div className={styles.profile_meta}>
                          <div className={styles.profile_title_line}>
                            <strong>{profile.name}</strong>
                            {isActive ? <span className={styles.active_tag}>当前使用</span> : null}
                          </div>
                          <div className={styles.profile_sub}>{subtitleParts.join(" · ")}</div>
                        </div>
                      </div>
                      <div className={styles.profile_actions}>
                        {!isActive ? (
                          <button
                            aria-label={`启用 ASR ${profile.name}`}
                            className={styles.row_btn_primary}
                            disabled={busy}
                            onClick={() => void gateway.setActiveSpeechProfile("asr", profile.id)}
                            type="button"
                          >
                            启用
                          </button>
                        ) : null}
                        <button
                          aria-expanded={isTesting}
                          aria-label={`测试 ASR ${profile.name}`}
                          className={styles.row_btn}
                          disabled={busy}
                          onClick={() => setTestingAsrId(isTesting ? null : profile.id)}
                          type="button"
                        >
                          {isTesting ? "收起" : "测试"}
                        </button>
                        <button
                          aria-label={`编辑 ASR ${profile.name}`}
                          className={styles.row_btn}
                          disabled={busy}
                          onClick={() => {
                            setTestingAsrId(null);
                            setAsrEditor({ mode: "edit", profile });
                          }}
                          type="button"
                        >
                          编辑
                        </button>
                        <button
                          aria-label={`删除 ASR ${profile.name}`}
                          className={styles.row_btn_danger}
                          disabled={busy}
                          onClick={() =>
                            setPendingDelete({ capability: "asr", id: profile.id, name: profile.name })
                          }
                          type="button"
                        >
                          删除
                        </button>
                      </div>
                    </div>
                    {isTesting ? (
                      <AsrQuickTestBar
                        key={profile.id}
                        loopback_tts={loopbackTts}
                        profile={profile}
                      />
                    ) : null}
                  </div>
                );
              })}
            </div>
          ) : !asrEditor ? (
            <p className={styles.empty_list}>暂无已保存的语音识别配置</p>
          ) : null}

          {asrEditor ? (
            <AsrProfileEditor
              default_activate={asrProfiles.length === 0 || !activeAsrId}
              is_active={asrEditor.mode === "edit" && asrEditor.profile.id === activeAsrId}
              key={asrEditor.mode === "edit" ? asrEditor.profile.id : "new-asr"}
              loopback_tts={loopbackTts}
              on_close={() => setAsrEditor(null)}
              profile={asrEditor.mode === "edit" ? asrEditor.profile : null}
            />
          ) : null}
        </article>

        {/* 语音合成 (TTS) 配置列表 */}
        <article className={styles.speech_card}>
          <div className={styles.speech_card_header}>
            <div className={styles.speech_card_title}>
              <strong>语音合成 (TTS)</strong>
              <span className={styles.status_badge} data-status={liveStatus?.tts}>
                {speechStatusLabel(liveStatus?.tts)}
              </span>
            </div>
            <div className={styles.speech_card_controls}>
              <label
                className={styles.switch}
                data-disabled={busy || ttsProfiles.length === 0}
              >
                <input
                  aria-label="启用语音合成"
                  checked={Boolean(activeTtsId)}
                  disabled={busy || ttsProfiles.length === 0}
                  onChange={(event) => void toggleTtsMaster(event.currentTarget.checked)}
                  type="checkbox"
                />
                <span />
                <b>{activeTtsId ? "已启用" : "已停用"}</b>
              </label>
              <button
                aria-label="新增语音合成配置"
                className={styles.add_btn}
                disabled={busy}
                onClick={() => {
                  setTestingTtsId(null);
                  setTtsEditor({ mode: "create" });
                }}
                type="button"
              >
                <Icon name="plus" size={13} />
                新增配置
              </button>
            </div>
          </div>

          {ttsProfiles.length > 0 ? (
            <div className={styles.profile_list}>
              {ttsProfiles.map((profile) => {
                const isActive = profile.id === activeTtsId;
                const presetId = detectTtsPreset(profile.provider, profile.endpoint);
                const meta = TTS_PRESETS[presetId];
                const isTesting = testingTtsId === profile.id;
                const subtitleParts = [
                  shortLabel(meta.label),
                  profile.voice,
                  profile.model,
                  profile.credential_masked,
                ].filter(Boolean);
                return (
                  <div
                    className={styles.profile_item}
                    data-active={isActive}
                    key={profile.id}
                  >
                    <div className={styles.profile_row}>
                      <div className={styles.profile_main}>
                        <span className={styles.profile_dot} data-active={isActive} />
                        <ProviderIcon name={meta.icon} size={18} />
                        <div className={styles.profile_meta}>
                          <div className={styles.profile_title_line}>
                            <strong>{profile.name}</strong>
                            {isActive ? <span className={styles.active_tag}>当前使用</span> : null}
                          </div>
                          <div className={styles.profile_sub}>{subtitleParts.join(" · ")}</div>
                        </div>
                      </div>
                      <div className={styles.profile_actions}>
                        {!isActive ? (
                          <button
                            aria-label={`启用 TTS ${profile.name}`}
                            className={styles.row_btn_primary}
                            disabled={busy}
                            onClick={() => void gateway.setActiveSpeechProfile("tts", profile.id)}
                            type="button"
                          >
                            启用
                          </button>
                        ) : null}
                        <button
                          aria-expanded={isTesting}
                          aria-label={`试听 TTS ${profile.name}`}
                          className={styles.row_btn}
                          disabled={busy}
                          onClick={() => setTestingTtsId(isTesting ? null : profile.id)}
                          type="button"
                        >
                          {isTesting ? "收起" : "试听"}
                        </button>
                        <button
                          aria-label={`编辑 TTS ${profile.name}`}
                          className={styles.row_btn}
                          disabled={busy}
                          onClick={() => {
                            setTestingTtsId(null);
                            setTtsEditor({ mode: "edit", profile });
                          }}
                          type="button"
                        >
                          编辑
                        </button>
                        <button
                          aria-label={`删除 TTS ${profile.name}`}
                          className={styles.row_btn_danger}
                          disabled={busy}
                          onClick={() =>
                            setPendingDelete({ capability: "tts", id: profile.id, name: profile.name })
                          }
                          type="button"
                        >
                          删除
                        </button>
                      </div>
                    </div>
                    {isTesting ? (
                      <TtsQuickPreviewBar key={profile.id} profile={profile} />
                    ) : null}
                  </div>
                );
              })}
            </div>
          ) : !ttsEditor ? (
            <p className={styles.empty_list}>暂无已保存的语音合成配置</p>
          ) : null}

          {ttsEditor ? (
            <TtsProfileEditor
              active_asr_id={activeAsrId}
              asr_profiles={asrProfiles}
              default_activate={ttsProfiles.length === 0 || !activeTtsId}
              is_active={ttsEditor.mode === "edit" && ttsEditor.profile.id === activeTtsId}
              key={ttsEditor.mode === "edit" ? ttsEditor.profile.id : "new-tts"}
              on_close={() => setTtsEditor(null)}
              profile={ttsEditor.mode === "edit" ? ttsEditor.profile : null}
            />
          ) : null}
        </article>
      </div>

      <PresenceBoundary present={pendingDelete !== null}>
        {pendingDelete ? (
          <SessionActionDialog
            confirm_label="删除"
            is_danger
            is_pending={busy}
            on_cancel={() => setPendingDelete(null)}
            on_confirm={() => void confirmDelete()}
            title={pendingDelete.capability === "asr" ? "删除语音识别配置？" : "删除语音合成配置？"}
          >
            <p>
              确定删除 <strong>{pendingDelete.name}</strong>？已保存的 API Key 将一并移除。
            </p>
          </SessionActionDialog>
        ) : null}
      </PresenceBoundary>
    </section>
  );
});
