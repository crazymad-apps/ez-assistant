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
  standalone_spacing?: boolean;
}>;

const MODE_OPTIONS: ReadonlyArray<Readonly<{
  value: WakeMode;
  title: string;
  requires_vp: boolean;
}>> = [
  { value: "wake_word", title: "唤醒词", requires_vp: false },
  { value: "wake_word_plus_vp", title: "唤醒词 + 声纹", requires_vp: true },
  { value: "voiceprint_direct", title: "声纹免唤醒", requires_vp: true },
];

const SENSITIVITY_OPTIONS: ReadonlyArray<Readonly<{
  label: string;
  threshold: number;
}>> = [
  { label: "严格", threshold: 0.85 },
  { label: "均衡", threshold: 0.78 },
  { label: "灵敏", threshold: 0.70 },
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
      setError(err instanceof Error ? err.message : "读取唤醒配置失败。");
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
      setError("请先录入声纹后再开启声纹唤醒。");
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
      setError("请先录入声纹后再开启声纹唤醒。");
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
      setWakePinyin(pinyinCheck.normalized);
      setNotice(res.synced === true ? "已同步生效。" : "已保存，终端上线后同步。");
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "保存唤醒设置失败。");
    } finally {
      setSaving(false);
    }
  }

  const showPinyinEditor = manualPinyin || unknownChars.length > 0 || (!pinyinCheck.valid && wakePinyin.trim().length > 0);

  return (
    <section
      aria-label="语音唤醒"
      className={props.standalone_spacing ? `${styles.panel} ${styles.panel_standalone}` : styles.panel}
    >
      <header className={styles.panel_header}>
        <div className={styles.action_row}>
          <h4>语音唤醒 · {props.device_name}</h4>
          <span className={styles.badge} data-tone={enrolledCount > 0 ? "online" : "warn"}>
            {enrolledCount > 0 ? `已录 ${enrolledCount} 人` : "未录声纹"}
          </span>
        </div>
        {props.on_open_voiceprint ? (
          <button onClick={props.on_open_voiceprint} type="button">
            {enrolledCount > 0 ? "管理声纹" : "录入声纹"}
          </button>
        ) : null}
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
                去录入 →
              </button>
            ) : null}
          </div>
        ) : null}
        {notice ? (
          <p className={styles.success_banner} role="status">
            {notice}
          </p>
        ) : null}

        <div className={styles.setting_row}>
          <span className={styles.setting_label}>唤醒模式</span>
          <div aria-label="唤醒模式切换" className={styles.segmented_group} role="radiogroup">
            {MODE_OPTIONS.map((opt) => {
              const selected = mode === opt.value;
              const locked = opt.requires_vp && enrolledCount === 0;
              return (
                <button
                  aria-checked={selected}
                  className={styles.segmented_btn}
                  data-locked={locked ? "true" : "false"}
                  data-selected={selected ? "true" : "false"}
                  key={opt.value}
                  onClick={() => handleModeSelect(opt.value)}
                  role="radio"
                  type="button"
                >
                  {opt.title}
                </button>
              );
            })}
          </div>
        </div>

        <div className={styles.setting_row}>
          <label className={styles.setting_label} htmlFor={`wake-word-${props.device_id}`}>
            唤醒词
          </label>
          <div className={styles.setting_control_col}>
            <div className={styles.wake_input_row}>
              <input
                aria-label="唤醒词"
                className={styles.wake_input}
                id={`wake-word-${props.device_id}`}
                maxLength={24}
                onChange={(event) => handleWakeWordChange(event.currentTarget.value)}
                placeholder={DEFAULT_WAKE_WORD}
                value={wakeWord}
              />
              <button
                className={styles.inline_link_btn}
                onClick={() => setManualPinyin((prev) => !prev)}
                type="button"
              >
                {manualPinyin ? "收起拼音" : "微调拼音"}
              </button>
            </div>
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

        {showPinyinEditor ? (
          <div className={styles.setting_row}>
            <label className={styles.setting_label} htmlFor={`wake-pinyin-${props.device_id}`}>
              识别拼音
            </label>
            <div className={styles.setting_control_col}>
              <input
                aria-label="唤醒词拼音"
                className={styles.wake_input}
                id={`wake-pinyin-${props.device_id}`}
                onChange={(event) => {
                  setWakePinyin(event.currentTarget.value);
                  setUnknownChars([]);
                  setNotice(null);
                }}
                placeholder={DEFAULT_WAKE_PINYIN}
                value={wakePinyin}
              />
              {unknownChars.length > 0 ? (
                <p className={styles.warn_text}>
                  未识别字符：{unknownChars.join("、")}，请手动补全拼音
                </p>
              ) : !pinyinCheck.valid && wakePinyin.trim().length > 0 ? (
                <p className={styles.warn_text}>{pinyinCheck.error}</p>
              ) : null}
            </div>
          </div>
        ) : null}

        {mode !== "wake_word" ? (
          <div className={styles.setting_row}>
            <span className={styles.setting_label}>灵敏度</span>
            <div className={styles.setting_control_col}>
              <div className={styles.action_row}>
                <div className={styles.segmented_group}>
                  {SENSITIVITY_OPTIONS.map((opt) => (
                    <button
                      className={styles.segmented_btn}
                      data-selected={Math.abs(vpThreshold - opt.threshold) < 0.02 ? "true" : "false"}
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
                    <span>免唤醒静音亮屏</span>
                  </label>
                ) : null}
              </div>
            </div>
          </div>
        ) : null}

        <div className={styles.wake_footer}>
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
