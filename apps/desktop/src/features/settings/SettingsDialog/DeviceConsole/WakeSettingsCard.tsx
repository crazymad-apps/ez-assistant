import { observer } from "mobx-react-lite";
import { useCallback, useEffect, useState } from "react";
import { useRootStore } from "../../../../stores/RootStoreContext";
import {
  normalizeConsolePath,
  parseWakeSnapshot,
  type DeviceWakeConfig,
  type WakeMode,
} from "./api";
import {
  chineseWakeWordToPinyin,
  DEFAULT_WAKE_PINYIN,
  DEFAULT_WAKE_WORD,
  validateWakePinyin,
  WAKE_WORD_PRESETS,
} from "./wakePinyin";
import styles from "./index.module.scss";

type WakeSettingsCardProps = Readonly<{
  device_id: string;
  device_name: string;
  api_prefix: string;
  on_open_voiceprint?: () => void;
  refresh_token?: number;
}>;

const MODE_OPTIONS: ReadonlyArray<Readonly<{
  value: WakeMode;
  title: string;
  desc: string;
  requires_vp: boolean;
}>> = [
  {
    value: "wake_word",
    title: "唤醒词模式",
    desc: "说出唤醒词（默认「你好小元」）唤醒对话；已录声纹时自动识别说话人身份。",
    requires_vp: false,
  },
  {
    value: "wake_word_plus_vp",
    title: "唤醒词 + 声纹锁定",
    desc: "必须说出唤醒词且匹配已录入声纹才响应，防止陌生人或电视误唤醒。",
    requires_vp: true,
  },
  {
    value: "voiceprint_direct",
    title: "声纹免唤醒",
    desc: "录入声纹后无需喊唤醒词，直接开口说话即可自动触发对话（首句不丢字）。",
    requires_vp: true,
  },
];

const SENSITIVITY_OPTIONS: ReadonlyArray<Readonly<{
  label: string;
  threshold: number;
  hint: string;
}>> = [
  { label: "严格 (0.85)", threshold: 0.85, hint: "防误触发优先，适合客厅或嘈杂环境" },
  { label: "均衡 (0.78)", threshold: 0.78, hint: "推荐默认值，兼顾召回率与抗误触" },
  { label: "灵敏 (0.70)", threshold: 0.70, hint: "更易直唤，适合安静书房个人使用" },
];

export const WakeSettingsCard = observer(function WakeSettingsCard(props: WakeSettingsCardProps) {
  const { device_gateway: gateway } = useRootStore();
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const [mode, setMode] = useState<WakeMode>("wake_word");
  const [wakeWord, setWakeWord] = useState(DEFAULT_WAKE_WORD);
  const [wakePinyin, setWakePinyin] = useState(DEFAULT_WAKE_PINYIN);
  const [manualPinyin, setManualPinyin] = useState(false);
  const [vpThreshold, setVpThreshold] = useState(0.78);
  const [vpWakeChime, setVpWakeChime] = useState(false);
  const [enrolledCount, setEnrolledCount] = useState(0);
  const [unknownChars, setUnknownChars] = useState<string[]>([]);

  const loadWakeConfig = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const payload = await gateway.consoleJson<unknown>(
        props.device_id,
        normalizeConsolePath(props.api_prefix, "wake"),
      );
      const snap = parseWakeSnapshot(payload);
      setMode(snap.wake.mode);
      setWakeWord(snap.wake.wake_word);
      setWakePinyin(snap.wake.wake_pinyin);
      setVpThreshold(snap.wake.vp_threshold);
      setVpWakeChime(snap.wake.vp_wake_chime);
      setEnrolledCount(snap.enrolled_speaker_count);
      setUnknownChars([]);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "读取终端唤醒配置失败。");
    } finally {
      setLoading(false);
    }
  }, [gateway, props.api_prefix, props.device_id]);

  useEffect(() => {
    void loadWakeConfig();
  }, [loadWakeConfig, props.refresh_token]);

  function handleWakeWordChange(nextWord: string) {
    setWakeWord(nextWord);
    setNotice(null);
    if (!manualPinyin) {
      const conv = chineseWakeWordToPinyin(nextWord);
      setWakePinyin(conv.pinyin);
      setUnknownChars(conv.unknown_chars);
    }
  }

  function applyPreset(word: string, pinyin: string) {
    setWakeWord(word);
    setWakePinyin(pinyin);
    setManualPinyin(false);
    setUnknownChars([]);
    setError(null);
    setNotice(null);
  }

  function handleModeSelect(nextMode: WakeMode) {
    setError(null);
    setNotice(null);
    if (nextMode !== "wake_word" && enrolledCount === 0) {
      setError("开启声纹唤醒前，请先为至少 1 位成员录入声纹。");
      return;
    }
    setMode(nextMode);
  }

  const pinyinCheck = validateWakePinyin(wakePinyin);

  async function saveWakeSettings(event: React.FormEvent) {
    event.preventDefault();
    setError(null);
    setNotice(null);

    if (mode !== "wake_word" && enrolledCount === 0) {
      setError("请先录入至少 1 位说话人声纹，再开启声纹唤醒模式。");
      return;
    }
    if (!pinyinCheck.valid) {
      setError(pinyinCheck.error ?? "唤醒词拼音格式无效。");
      return;
    }

    setSaving(true);
    try {
      const body: Partial<DeviceWakeConfig> = {
        mode,
        wake_word: wakeWord.trim() || DEFAULT_WAKE_WORD,
        wake_pinyin: pinyinCheck.normalized,
        vp_threshold: vpThreshold,
        vp_wake_chime: vpWakeChime,
      };
      const res = await gateway.consoleJson<Record<string, unknown>>(
        props.device_id,
        normalizeConsolePath(props.api_prefix, "wake"),
        {
          method: "PUT",
          body,
        },
      );
      const synced = res.synced === true;
      const modeText =
        mode === "voiceprint_direct"
          ? "声纹免唤醒模式"
          : mode === "wake_word_plus_vp"
            ? `唤醒词「${wakeWord.trim() || DEFAULT_WAKE_WORD}」+ 声纹锁定`
            : `唤醒词「${wakeWord.trim() || DEFAULT_WAKE_WORD}」`;
      setNotice(
        synced
          ? `已热更新至 ${props.device_name}（${modeText}，无需重启）。`
          : `配置已保存（${modeText}），将在终端重新上线后自动同步。`,
      );
      setWakePinyin(pinyinCheck.normalized);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "保存唤醒设置失败。");
    } finally {
      setSaving(false);
    }
  }

  const activeModeMeta = MODE_OPTIONS.find((opt) => opt.value === mode) ?? MODE_OPTIONS[0];
  const activeSensitivity =
    SENSITIVITY_OPTIONS.reduce((best, item) =>
      Math.abs(item.threshold - vpThreshold) < Math.abs(best.threshold - vpThreshold) ? item : best,
    );

  return (
    <section aria-label="语音唤醒与模式" className={styles.panel}>
      <header className={styles.panel_header}>
        <div className={styles.action_row}>
          <h4>语音唤醒与模式 · {props.device_name}</h4>
          <span
            className={styles.badge}
            data-tone={enrolledCount > 0 ? "online" : "warn"}
          >
            {enrolledCount > 0 ? `已录 ${enrolledCount} 人声纹` : "未录声纹"}
          </span>
        </div>
        <div className={styles.action_row}>
          {props.on_open_voiceprint ? (
            <button
              className={styles.secondary_btn}
              onClick={props.on_open_voiceprint}
              type="button"
            >
              {enrolledCount > 0 ? "管理声纹" : "立即录入声纹"}
            </button>
          ) : null}
          <button
            disabled={loading || saving}
            onClick={() => void loadWakeConfig()}
            type="button"
          >
            {loading ? "读取中…" : "刷新"}
          </button>
        </div>
      </header>

      <form className={styles.panel_body} onSubmit={(event) => void saveWakeSettings(event)}>
        {error ? (
          <div className={styles.error_banner} role="alert">
            <span>{error}</span>
            {enrolledCount === 0 && props.on_open_voiceprint ? (
              <button
                className={styles.inline_link_btn}
                onClick={props.on_open_voiceprint}
                type="button"
              >
                去录入声纹 →
              </button>
            ) : null}
          </div>
        ) : null}
        {notice ? (
          <p className={styles.success_banner} role="status">
            {notice}
          </p>
        ) : null}

        <div className={styles.field}>
          <span>唤醒模式</span>
          <div aria-label="唤醒模式切换" className={styles.mode_grid} role="radiogroup">
            {MODE_OPTIONS.map((opt) => {
              const selected = mode === opt.value;
              const locked = opt.requires_vp && enrolledCount === 0;
              return (
                <button
                  aria-checked={selected}
                  className={styles.mode_card}
                  data-locked={locked ? "true" : "false"}
                  data-selected={selected ? "true" : "false"}
                  key={opt.value}
                  onClick={() => handleModeSelect(opt.value)}
                  role="radio"
                  type="button"
                >
                  <strong>{opt.title}</strong>
                  <small>{opt.desc}</small>
                  {locked ? <span className={styles.lock_hint}>需先录入声纹</span> : null}
                </button>
              );
            })}
          </div>
        </div>

        <div className={styles.form_grid}>
          <label className={styles.field}>
            <span>自定义唤醒词（默认：你好小元）</span>
            <input
              aria-label="唤醒词"
              maxLength={24}
              onChange={(event) => handleWakeWordChange(event.currentTarget.value)}
              placeholder={DEFAULT_WAKE_WORD}
              value={wakeWord}
            />
          </label>

          <div className={styles.field}>
            <span>常用唤醒词预设</span>
            <div className={styles.action_row}>
              {WAKE_WORD_PRESETS.map((preset) => (
                <button
                  className={styles.preset_chip}
                  data-active={wakeWord.trim() === preset.word ? "true" : "false"}
                  key={preset.word}
                  onClick={() => applyPreset(preset.word, preset.pinyin)}
                  type="button"
                >
                  {preset.word}
                </button>
              ))}
            </div>
          </div>
        </div>

        <div className={styles.pinyin_bar}>
          <div className={styles.meta_line}>
            MultiNet7 识别拼音：<code>{wakePinyin || "（未生成）"}</code>
            {unknownChars.length > 0 ? (
              <span className={styles.warn_text}>
                {" "}· 未自动识别字符：{unknownChars.join("、")}，请点击右侧手动微调
              </span>
            ) : null}
          </div>
          <button
            className={styles.inline_link_btn}
            onClick={() => setManualPinyin((prev) => !prev)}
            type="button"
          >
            {manualPinyin ? "收起拼音编辑" : "手动微调拼音"}
          </button>
        </div>

        {manualPinyin ? (
          <label className={styles.field}>
            <span>MultiNet7 拼音（2～8 个无声调小写音节，空格分隔）</span>
            <input
              aria-label="唤醒词拼音"
              onChange={(event) => {
                setWakePinyin(event.currentTarget.value);
                setUnknownChars([]);
                setNotice(null);
              }}
              placeholder={DEFAULT_WAKE_PINYIN}
              value={wakePinyin}
            />
          </label>
        ) : null}

        {!pinyinCheck.valid && wakePinyin.trim().length > 0 ? (
          <p className={styles.warn_text}>{pinyinCheck.error}</p>
        ) : null}

        {mode !== "wake_word" ? (
          <div className={styles.section_grid}>
            <div className={styles.field}>
              <span>声纹唤醒灵敏度（当前：{activeSensitivity.label} · {activeSensitivity.hint}）</span>
              <div className={styles.action_row}>
                {SENSITIVITY_OPTIONS.map((opt) => (
                  <button
                    className={styles.preset_chip}
                    data-active={Math.abs(vpThreshold - opt.threshold) < 0.02 ? "true" : "false"}
                    key={opt.label}
                    onClick={() => {
                      setVpThreshold(opt.threshold);
                      setNotice(null);
                    }}
                    type="button"
                  >
                    {opt.label}
                  </button>
                ))}
              </div>
            </div>
            {mode === "voiceprint_direct" ? (
              <label className={styles.check_row}>
                <input
                  checked={!vpWakeChime}
                  onChange={(event) => {
                    setVpWakeChime(!event.currentTarget.checked);
                    setNotice(null);
                  }}
                  type="checkbox"
                />
                <span>声纹免唤醒时保持静音亮屏（不播提示音，避免打断你正在说的话）</span>
              </label>
            ) : null}
          </div>
        ) : (
          <p className={styles.meta_line}>
            当前模式：{activeModeMeta.title}。
            {enrolledCount === 0
              ? "录入声纹后可一键开启「声纹免唤醒」，直接说话即可对话。"
              : "已检测到已录声纹，可随时切换至「声纹免唤醒」。"}
          </p>
        )}

        <div className={styles.action_row}>
          <button
            className={styles.primary_btn}
            disabled={saving || loading || !pinyinCheck.valid}
            type="submit"
          >
            {saving ? "同步中…" : "保存唤醒设置"}
          </button>
        </div>
      </form>
    </section>
  );
});
