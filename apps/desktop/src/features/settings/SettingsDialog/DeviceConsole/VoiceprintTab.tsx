import { observer } from "mobx-react-lite";
import { useCallback, useEffect, useRef, useState } from "react";
import { SelectionPopover, type SelectionOption } from "../../../../components/SelectionPopover";
import { useRootStore } from "../../../../stores/RootStoreContext";
import {
  normalizeConsolePath,
  parseSpeakers,
  pickNumber,
  pickString,
  roleLabel,
  type CapturedVoiceprintSample,
  type SpeakerGroupEntry,
} from "./api";
import { WakeSettingsCard } from "./WakeSettingsCard";
import {
  BrowserWavRecorder,
  synthesizeTestToneSample,
  type RecordedAudioSample,
} from "./wavRecorder";
import styles from "./index.module.scss";

type VoiceprintTabProps = Readonly<{
  device_id: string;
  device_name?: string;
  api_prefix: string;
  mode?: "proxy" | "native";
}>;

const GUIDED_SENTENCES: ReadonlyArray<string> = [
  "你好小元，今天天气怎么样？",
  "帮我看一下现在的温度和电量。",
  "明天早上八点提醒我开会。",
];

const ROLE_OPTIONS: ReadonlyArray<SelectionOption<string>> = [
  { value: "1", label: "Owner (1)" },
  { value: "2", label: "Family (2)" },
  { value: "3", label: "Guest (3)" },
];

function classifyConfidence(confidence: number, speakerId: string): { label: string; tone: "online" | "warn" | "offline" } {
  if (confidence >= 0.85) {
    return { label: `${speakerId} (本地高可信)`, tone: "online" };
  }
  if (confidence >= 0.65 && speakerId) {
    return { label: `${speakerId} (灰区复核)`, tone: "warn" };
  }
  return { label: "未匹配 / Guest", tone: "offline" };
}

export const VoiceprintTab = observer(function VoiceprintTab(props: VoiceprintTabProps) {
  const { device_gateway: gateway } = useRootStore();
  const [speakers, setSpeakers] = useState<SpeakerGroupEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [speakerId, setSpeakerId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [uid, setUid] = useState("uid-owner-1");
  const [agentId, setAgentId] = useState("ez-assistant");
  const [groupId, setGroupId] = useState("home-default");
  const [role, setRole] = useState("1");
  const [ttsVoice, setTtsVoice] = useState("longxiaochun");
  const [prompt, setPrompt] = useState("");
  const [roleOpen, setRoleOpen] = useState(false);
  const [savingSpeaker, setSavingSpeaker] = useState(false);

  const recorderRef = useRef<BrowserWavRecorder | null>(null);
  const [recording, setRecording] = useState(false);
  const [sample, setSample] = useState<RecordedAudioSample | null>(null);
  const [deviceSamples, setDeviceSamples] = useState<CapturedVoiceprintSample[]>([]);
  const [capturingDevice, setCapturingDevice] = useState(false);
  const [wakeRefreshToken, setWakeRefreshToken] = useState(0);
  const [notice, setNotice] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState(false);
  const [voiceResult, setVoiceResult] = useState<string | null>(null);
  const [confidence, setConfidence] = useState<number | null>(null);
  const [matchedSpeaker, setMatchedSpeaker] = useState("");
  const isNative = props.mode === "native";

  useEffect(() => {
    return () => {
      recorderRef.current?.dispose();
    };
  }, []);

  const refreshSpeakers = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const payload = await gateway.consoleJson<unknown>(
        props.device_id,
        normalizeConsolePath(props.api_prefix, "speakers"),
      );
      setSpeakers(parseSpeakers(payload));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "加载声纹成员失败。");
    } finally {
      setLoading(false);
    }
  }, [gateway, props.api_prefix, props.device_id]);

  useEffect(() => {
    void refreshSpeakers();
  }, [refreshSpeakers]);

  async function saveSpeakerGroup(event: React.FormEvent) {
    event.preventDefault();
    const cleanedId = speakerId.trim();
    if (!cleanedId) {
      setError("请填写说话人 ID。");
      return;
    }
    setSavingSpeaker(true);
    setError(null);
    try {
      await gateway.consoleJson(
        props.device_id,
        normalizeConsolePath(props.api_prefix, `speakers/${encodeURIComponent(cleanedId)}`),
        {
          method: "PUT",
          body: {
            display_name: displayName.trim() || cleanedId,
            uid: uid.trim(),
            agent_id: agentId.trim(),
            group_id: groupId.trim(),
            role: Number(role),
            tts_voice: ttsVoice.trim(),
            prompt: prompt.trim(),
          },
        },
      );
      await refreshSpeakers();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "保存声纹成员失败。");
    } finally {
      setSavingSpeaker(false);
    }
  }

  async function deleteSpeaker(id: string) {
    setError(null);
    setNotice(null);
    try {
      const res = await gateway.consoleJson<Record<string, unknown>>(
        props.device_id,
        normalizeConsolePath(props.api_prefix, `speakers/${encodeURIComponent(id)}`),
        { method: "DELETE" },
      );
      if (res.auto_fallback_wake_word === true) {
        setNotice("已删除最后一位声纹成员，终端已自动切回「唤醒词模式」以防无法唤醒。");
      }
      setWakeRefreshToken((n) => n + 1);
      await refreshSpeakers();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "删除说话人失败。");
    }
  }

  async function captureFromDevice() {
    if (deviceSamples.length >= 3) {
      setError("已采集满 3 句样本，可直接点击「声纹注册 (Enroll)」或先清空重录。");
      return;
    }
    const step = deviceSamples.length + 1;
    setCapturingDevice(true);
    setError(null);
    setNotice(`请靠近 ${props.device_name ?? "Mosaico"} 清晰说出第 ${step}/3 句：「${GUIDED_SENTENCES[step - 1]}」…`);
    try {
      const res = await gateway.consoleJson<Record<string, unknown>>(
        props.device_id,
        normalizeConsolePath(props.api_prefix, "voice/capture_embedding"),
        {
          method: "POST",
          body: { duration_ms: 3000, step, total: 3 },
        },
      );
      const embedding = Array.isArray(res.embedding)
        ? res.embedding.filter((v): v is number => typeof v === "number" && Number.isFinite(v))
        : [];
      if (embedding.length !== 192) {
        throw new Error("终端返回的声纹特征不完整，请重试。");
      }
      const next: CapturedVoiceprintSample = {
        embedding,
        voiced_ms: pickNumber(res, 0, "voiced_ms"),
        snr_db: pickNumber(res, 0, "snr_db"),
        rms: pickNumber(res, 0, "rms"),
        duration_ms: pickNumber(res, 3000, "duration_ms"),
        label: `第 ${step} 句`,
      };
      setDeviceSamples((prev) => [...prev, next]);
      setNotice(
        `第 ${step}/3 句采集完成（有效人声 ${(next.voiced_ms / 1000).toFixed(1)}s · SNR ${next.snr_db.toFixed(1)} dB）`,
      );
    } catch (err: unknown) {
      setNotice(null);
      setError(err instanceof Error ? err.message : "Mosaico 本机采集声纹失败。");
    } finally {
      setCapturingDevice(false);
    }
  }

  async function toggleRecording() {
    setError(null);
    if (recording) {
      try {
        const next = await recorderRef.current?.stop();
        if (next) setSample(next);
      } catch (err: unknown) {
        setError(err instanceof Error ? err.message : "停止录音失败。");
      } finally {
        setRecording(false);
      }
      return;
    }
    try {
      recorderRef.current ??= new BrowserWavRecorder();
      await recorderRef.current.start();
      setRecording(true);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "启动麦克风录音失败。");
      setRecording(false);
    }
  }

  async function enrollVoiceprint() {
    if (deviceSamples.length === 0 && !sample) {
      setError("请先用 Mosaico 录制声纹、电脑麦克风录音或生成合成测试样本。");
      return;
    }
    const cleanedId = speakerId.trim();
    if (!cleanedId) {
      setError("声纹注册需要填写说话人 ID。");
      return;
    }
    setBusyAction(true);
    setError(null);
    setNotice(null);
    try {
      const body: Record<string, unknown> = {
        speaker_id: cleanedId,
        display_name: displayName.trim() || cleanedId,
        uid: uid.trim(),
        agent_id: agentId.trim(),
        group_id: groupId.trim(),
        role: Number(role),
        tts_voice: ttsVoice.trim(),
        prompt: prompt.trim(),
      };
      if (deviceSamples.length > 0) {
        body.embeddings = deviceSamples.map((s) => s.embedding);
      } else if (sample) {
        body.audio_base64 = sample.base64;
      }
      const res = await gateway.consoleJson<Record<string, unknown>>(
        props.device_id,
        normalizeConsolePath(props.api_prefix, "voice/enroll"),
        {
          method: "POST",
          body,
        },
      );
      setVoiceResult(JSON.stringify(res, null, 2));
      setDeviceSamples([]);
      setNotice("声纹已注册并同步至终端加密 NVS，现在可开启「声纹免唤醒」模式。");
      setWakeRefreshToken((n) => n + 1);
      await refreshSpeakers();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "声纹注册失败。");
    } finally {
      setBusyAction(false);
    }
  }

  async function identifyVoiceprint() {
    if (deviceSamples.length === 0 && !sample) {
      setError("请先录制人声、用 Mosaico 采集或生成合成测试样本。");
      return;
    }
    setBusyAction(true);
    setError(null);
    try {
      const body: Record<string, unknown> = {
        uid: uid.trim(),
        agent_id: agentId.trim(),
      };
      if (deviceSamples.length > 0) {
        body.embedding = deviceSamples[deviceSamples.length - 1].embedding;
      } else if (sample) {
        body.audio_base64 = sample.base64;
      }
      const res = await gateway.consoleJson<Record<string, unknown>>(
        props.device_id,
        normalizeConsolePath(props.api_prefix, "voice/identify"),
        {
          method: "POST",
          body,
        },
      );
      setConfidence(pickNumber(res, 0, "confidence"));
      setMatchedSpeaker(pickString(res, "speakerId", "speaker_id"));
      setVoiceResult(JSON.stringify(res, null, 2));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "声纹识别失败。");
    } finally {
      setBusyAction(false);
    }
  }

  async function transcribeSample() {
    if (!sample) {
      setError("请先录制人声或点击生成合成测试样本。");
      return;
    }
    setBusyAction(true);
    setError(null);
    try {
      const res = await gateway.consoleJson<Record<string, unknown>>(
        props.device_id,
        normalizeConsolePath(props.api_prefix, "voice/transcribe"),
        {
          method: "POST",
          body: {
            device_id: props.device_id,
            audio_base64: sample.base64,
          },
        },
      );
      setVoiceResult(JSON.stringify(res, null, 2));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "语音转写测试失败。");
    } finally {
      setBusyAction(false);
    }
  }

  const tier = confidence !== null ? classifyConfidence(confidence, matchedSpeaker) : null;
  const pct = confidence !== null ? Math.max(0, Math.min(100, Math.round(confidence * 100))) : 0;
  const hasVoiceSample = deviceSamples.length > 0 || sample !== null;

  return (
    <div className={styles.section_grid}>
      {isNative ? (
        <WakeSettingsCard
          api_prefix={props.api_prefix}
          device_id={props.device_id}
          device_name={props.device_name ?? "Mosaico"}
          refresh_token={wakeRefreshToken}
        />
      ) : null}

      <section className={styles.panel}>
        <header className={styles.panel_header}>
          <h4>说话人配置与声纹采样</h4>
        </header>
        <form className={styles.panel_body} onSubmit={(event) => void saveSpeakerGroup(event)}>
          {error ? <p className={styles.error_banner} role="alert">{error}</p> : null}
          {notice ? <p className={styles.success_banner} role="status">{notice}</p> : null}
          <div className={styles.form_grid}>
            <label className={styles.field}>
              <span>说话人 ID (Speaker ID)</span>
              <input
                aria-label="说话人 ID"
                onChange={(event) => setSpeakerId(event.currentTarget.value)}
                placeholder="spk-owner-1"
                value={speakerId}
              />
            </label>

            <label className={styles.field}>
              <span>显示名称</span>
              <input
                aria-label="说话人显示名称"
                onChange={(event) => setDisplayName(event.currentTarget.value)}
                placeholder="主人"
                value={displayName}
              />
            </label>

            <label className={styles.field}>
              <span>用户 UID</span>
              <input
                aria-label="说话人 UID"
                onChange={(event) => setUid(event.currentTarget.value)}
                value={uid}
              />
            </label>

            <label className={styles.field}>
              <span>绑定 Agent ID</span>
              <input
                aria-label="绑定 Agent ID"
                onChange={(event) => setAgentId(event.currentTarget.value)}
                value={agentId}
              />
            </label>

            <label className={styles.field}>
              <span>所属组 ID (Group)</span>
              <input
                aria-label="所属组 ID"
                onChange={(event) => setGroupId(event.currentTarget.value)}
                value={groupId}
              />
            </label>

            <div className={styles.field}>
              <span>权限角色</span>
              <SelectionPopover
                aria_label="声纹角色"
                content_width="content"
                on_open_change={setRoleOpen}
                on_select={(val) => {
                  setRole(val);
                  setRoleOpen(false);
                }}
                open={roleOpen}
                options={ROLE_OPTIONS}
                selected={role}
                trigger_variant="field"
              />
            </div>

            <label className={styles.field}>
              <span>首选 TTS 音色</span>
              <input
                aria-label="TTS 音色"
                onChange={(event) => setTtsVoice(event.currentTarget.value)}
                value={ttsVoice}
              />
            </label>

            <label className={`${styles.field} ${styles.full_span}`}>
              <span>个性化提示词 (Prompt)</span>
              <textarea
                aria-label="说话人提示词"
                onChange={(event) => setPrompt(event.currentTarget.value)}
                placeholder="可选：为该说话人设定专属称呼或回复风格"
                value={prompt}
              />
            </label>
          </div>

          {isNative ? (
            <div className={styles.capture_box}>
              <div className={styles.action_row}>
                <strong>{props.device_name ?? "Mosaico"} 本机录制</strong>
                <span className={styles.badge} data-tone={deviceSamples.length >= 3 ? "online" : "warn"}>
                  {deviceSamples.length}/3 句
                </span>
              </div>
              {deviceSamples.length < 3 ? (
                <p className={styles.meta_line}>
                  建议朗读：「{GUIDED_SENTENCES[deviceSamples.length]}」
                </p>
              ) : null}
              <div className={styles.action_row}>
                <button
                  className={styles.primary_btn}
                  disabled={capturingDevice || busyAction || recording || deviceSamples.length >= 3}
                  onClick={() => void captureFromDevice()}
                  type="button"
                >
                  {capturingDevice
                    ? "Mosaico 正在收音…"
                    : deviceSamples.length >= 3
                      ? "已完成 3 句采集"
                      : `用 Mosaico 录第 ${deviceSamples.length + 1} 句`}
                </button>
                {deviceSamples.length > 0 ? (
                  <button
                    className={styles.secondary_btn}
                    disabled={capturingDevice || busyAction}
                    onClick={() => setDeviceSamples([])}
                    type="button"
                  >
                    清空终端采样
                  </button>
                ) : null}
              </div>
              {deviceSamples.length > 0 ? (
                <div className={styles.capture_list}>
                  {deviceSamples.map((item, idx) => (
                    <span className={styles.cap_tag} key={`${item.label}-${idx}`}>
                      {item.label} · 有效 {(item.voiced_ms / 1000).toFixed(1)}s · SNR {item.snr_db.toFixed(1)} dB
                    </span>
                  ))}
                </div>
              ) : null}
            </div>
          ) : null}

          <div className={styles.action_row}>
            <button className={styles.primary_btn} disabled={savingSpeaker} type="submit">
              {savingSpeaker ? "保存中…" : "保存说话人信息"}
            </button>
            <button
              className={styles.secondary_btn}
              disabled={busyAction || capturingDevice}
              onClick={() => void toggleRecording()}
              type="button"
            >
              {recording ? "停止录音 (16kHz WAV)" : "用电脑麦克风录制"}
            </button>
            <button
              className={styles.secondary_btn}
              disabled={busyAction || recording || capturingDevice}
              onClick={() => setSample(synthesizeTestToneSample(150))}
              type="button"
            >
              生成合成样本 150Hz
            </button>
            <button
              className={styles.secondary_btn}
              disabled={busyAction || recording || capturingDevice}
              onClick={() => setSample(synthesizeTestToneSample(420))}
              type="button"
            >
              生成合成样本 420Hz
            </button>
          </div>

          {sample ? (
            <div className={styles.meta_line}>
              当前电脑音频样本：{sample.label} · {sample.wav_bytes.byteLength} 字节
            </div>
          ) : null}

          <div className={styles.action_row}>
            <button
              className={styles.primary_btn}
              disabled={busyAction || capturingDevice || !hasVoiceSample}
              onClick={() => void enrollVoiceprint()}
              type="button"
            >
              声纹注册 (Enroll)
            </button>
            <button
              className={styles.secondary_btn}
              disabled={busyAction || capturingDevice || !hasVoiceSample}
              onClick={() => void identifyVoiceprint()}
              type="button"
            >
              声纹识别验证 (Identify)
            </button>
            <button
              className={styles.secondary_btn}
              disabled={busyAction || capturingDevice || !sample}
              onClick={() => void transcribeSample()}
              type="button"
            >
              ASR 转写测试 (Transcribe)
            </button>
          </div>

          {tier ? (
            <div className={styles.section_grid}>
              <div className={styles.action_row}>
                <span className={styles.badge} data-tone={tier.tone}>{tier.label}</span>
                <span className={styles.meta_line}>置信度 {pct}%</span>
              </div>
              <div className={styles.progress_track}>
                <div className={styles.progress_fill} style={{ width: `${pct}%` }} />
              </div>
            </div>
          ) : null}

          {voiceResult ? <pre className={styles.code_box}>{voiceResult}</pre> : null}
        </form>
      </section>

      <section className={styles.panel}>
        <header className={styles.panel_header}>
          <h4>已注册声纹成员 ({speakers.length})</h4>
          <button disabled={loading} onClick={() => void refreshSpeakers()} type="button">
            {loading ? "加载中…" : "刷新"}
          </button>
        </header>
        <div className={styles.panel_body}>
          {speakers.length > 0 ? (
            <div className={styles.table_wrap}>
              <table>
                <thead>
                  <tr>
                    <th>ID</th>
                    <th>名称</th>
                    <th>角色</th>
                    <th>Agent</th>
                    <th>TTS 音色</th>
                    <th>版本</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {speakers.map((sp) => (
                    <tr key={sp.speaker_id}>
                      <td><code>{sp.speaker_id}</code></td>
                      <td>{sp.display_name}</td>
                      <td>{roleLabel(sp.role)}</td>
                      <td>{sp.agent_id}</td>
                      <td><code>{sp.tts_voice || "-"}</code></td>
                      <td>{sp.version}</td>
                      <td>
                        <button
                          className={styles.danger_btn}
                          onClick={() => void deleteSpeaker(sp.speaker_id)}
                          type="button"
                        >
                          删除
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            <p className={styles.empty_note}>暂无已注册说话人。</p>
          )}
        </div>
      </section>
    </div>
  );
});
