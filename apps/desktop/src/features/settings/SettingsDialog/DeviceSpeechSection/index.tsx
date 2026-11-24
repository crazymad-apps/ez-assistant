import { observer } from "mobx-react-lite";
import { useEffect, useState } from "react";
import type {
  SpeechConfigurationSnapshot,
  SpeechProviderKind,
  SpeechServiceStatusSnapshot,
} from "@ez-assistant/protocol";
import { SelectionPopover, type SelectionOption } from "../../../../components/SelectionPopover";
import { useRootStore } from "../../../../stores/RootStoreContext";
import shellStyles from "../index.module.scss";
import styles from "./index.module.scss";

const PROVIDER_OPTIONS: ReadonlyArray<SelectionOption<SpeechProviderKind>> = [
  { value: "dashscope", label: "阿里云百炼 (DashScope)" },
  { value: "openai", label: "OpenAI 兼容接口" },
];

type AsrDraft = {
  enabled: boolean;
  provider: SpeechProviderKind;
  endpoint: string;
  model: string;
  timeout_ms: string;
  credential: string;
  clear_credential: boolean;
};

type TtsDraft = {
  enabled: boolean;
  provider: SpeechProviderKind;
  endpoint: string;
  model: string;
  voice: string;
  timeout_ms: string;
  credential: string;
  clear_credential: boolean;
};

function defaultEndpoint(provider: SpeechProviderKind): string {
  return provider === "openai" ? "https://api.openai.com/v1" : "https://dashscope.aliyuncs.com";
}

function defaultAsrModel(provider: SpeechProviderKind): string {
  return provider === "openai" ? "gpt-4o-mini-transcribe" : "fun-asr-realtime";
}

function defaultTtsModel(provider: SpeechProviderKind): string {
  return provider === "openai" ? "gpt-4o-mini-tts" : "cosyvoice-v3-flash";
}

function defaultTtsVoice(provider: SpeechProviderKind): string {
  return provider === "openai" ? "alloy" : "longxiaochun";
}

function speechStatusLabel(status?: SpeechServiceStatusSnapshot): string {
  if (status === "ready") return "可用";
  if (status === "degraded") return "部分可用";
  if (status === "unavailable") return "不可用";
  return "待同步";
}

function toAsrDraft(config: SpeechConfigurationSnapshot): AsrDraft {
  return {
    enabled: config.asr.enabled,
    provider: config.asr.provider,
    endpoint: config.asr.endpoint,
    model: config.asr.model,
    timeout_ms: String(config.asr.timeout_ms),
    credential: "",
    clear_credential: false,
  };
}

function toTtsDraft(config: SpeechConfigurationSnapshot): TtsDraft {
  return {
    enabled: config.tts.enabled,
    provider: config.tts.provider,
    endpoint: config.tts.endpoint,
    model: config.tts.model,
    voice: config.tts.voice,
    timeout_ms: String(config.tts.timeout_ms),
    credential: "",
    clear_credential: false,
  };
}

function emptyAsrDraft(): AsrDraft {
  return {
    enabled: false,
    provider: "dashscope",
    endpoint: defaultEndpoint("dashscope"),
    model: defaultAsrModel("dashscope"),
    timeout_ms: "30000",
    credential: "",
    clear_credential: false,
  };
}

function emptyTtsDraft(): TtsDraft {
  return {
    enabled: false,
    provider: "dashscope",
    endpoint: defaultEndpoint("dashscope"),
    model: defaultTtsModel("dashscope"),
    voice: defaultTtsVoice("dashscope"),
    timeout_ms: "30000",
    credential: "",
    clear_credential: false,
  };
}

function parseTimeoutMs(raw: string): number {
  const parsed = Number(raw);
  if (!Number.isFinite(parsed) || parsed < 1) return 30000;
  return Math.min(120000, Math.round(parsed));
}

export const DeviceSpeechSection = observer(function DeviceSpeechSection() {
  const store = useRootStore();
  const gateway = store.device_gateway;
  const config = gateway.speech_config;
  const liveStatus = gateway.snapshot?.speech_services;

  const [asr, setAsr] = useState<AsrDraft>(emptyAsrDraft);
  const [tts, setTts] = useState<TtsDraft>(emptyTtsDraft);
  const [asrProviderOpen, setAsrProviderOpen] = useState(false);
  const [ttsProviderOpen, setTtsProviderOpen] = useState(false);
  const [syncedKey, setSyncedKey] = useState<string | null>(null);

  useEffect(() => {
    if (!config) return;
    const key = `${config.revision ?? "none"}:${config.asr.enabled}:${config.tts.enabled}:${config.asr.provider}:${config.tts.provider}`;
    if (key !== syncedKey) {
      setAsr(toAsrDraft(config));
      setTts(toTtsDraft(config));
      setSyncedKey(key);
    }
  }, [config, syncedKey]);

  const busy = gateway.pending_action !== null || gateway.speech_loading;

  function handleAsrProviderChange(next: SpeechProviderKind) {
    setAsrProviderOpen(false);
    setAsr((prev) => {
      const nextEndpoint =
        !prev.endpoint.trim() || prev.endpoint === defaultEndpoint(prev.provider)
          ? defaultEndpoint(next)
          : prev.endpoint;
      const nextModel =
        !prev.model.trim() || prev.model === defaultAsrModel(prev.provider)
          ? defaultAsrModel(next)
          : prev.model;
      return { ...prev, provider: next, endpoint: nextEndpoint, model: nextModel };
    });
  }

  function handleTtsProviderChange(next: SpeechProviderKind) {
    setTtsProviderOpen(false);
    setTts((prev) => {
      const nextEndpoint =
        !prev.endpoint.trim() || prev.endpoint === defaultEndpoint(prev.provider)
          ? defaultEndpoint(next)
          : prev.endpoint;
      const nextModel =
        !prev.model.trim() || prev.model === defaultTtsModel(prev.provider)
          ? defaultTtsModel(next)
          : prev.model;
      const nextVoice =
        !prev.voice.trim() || prev.voice === defaultTtsVoice(prev.provider)
          ? defaultTtsVoice(next)
          : prev.voice;
      return { ...prev, provider: next, endpoint: nextEndpoint, model: nextModel, voice: nextVoice };
    });
  }

  async function saveConfiguration() {
    await gateway.updateSpeechConfiguration({
      expected_revision: config?.revision ?? undefined,
      asr: {
        enabled: asr.enabled,
        provider: asr.provider,
        endpoint: asr.endpoint.trim(),
        model: asr.model.trim(),
        timeout_ms: parseTimeoutMs(asr.timeout_ms),
        credential: asr.credential.trim() ? asr.credential.trim() : undefined,
        clear_credential: asr.clear_credential,
      },
      tts: {
        enabled: tts.enabled,
        provider: tts.provider,
        endpoint: tts.endpoint.trim(),
        model: tts.model.trim(),
        voice: tts.voice.trim(),
        timeout_ms: parseTimeoutMs(tts.timeout_ms),
        credential: tts.credential.trim() ? tts.credential.trim() : undefined,
        clear_credential: tts.clear_credential,
      },
    });
  }

  const asrCredentialHint = config?.asr.credential_configured
    ? `已配置凭据 ${config.asr.credential_masked}`
    : "尚未配置凭据";
  const ttsCredentialHint = config?.tts.credential_configured
    ? `已配置凭据 ${config.tts.credential_masked}`
    : "尚未配置凭据";

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
            <div className={styles.field}>
              <span>服务提供商</span>
              <SelectionPopover<SpeechProviderKind>
                aria_label="ASR 服务提供商"
                content_width="content"
                disabled={busy}
                on_open_change={setAsrProviderOpen}
                on_select={handleAsrProviderChange}
                open={asrProviderOpen}
                options={PROVIDER_OPTIONS}
                selected={asr.provider}
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
              <span>接口地址（Endpoint）</span>
              <input
                aria-label="ASR 接口地址"
                disabled={busy}
                onChange={(event) => setAsr({ ...asr, endpoint: event.currentTarget.value })}
                placeholder={defaultEndpoint(asr.provider)}
                value={asr.endpoint}
              />
            </label>

            <label className={`${styles.field} ${styles.full_width}`}>
              <span>模型名称</span>
              <input
                aria-label="ASR 模型名称"
                disabled={busy}
                onChange={(event) => setAsr({ ...asr, model: event.currentTarget.value })}
                placeholder={defaultAsrModel(asr.provider)}
                value={asr.model}
              />
            </label>

            <div className={`${styles.field} ${styles.full_width}`}>
              <div className={styles.credential_row}>
                <span>API Key 凭据</span>
                <span className={styles.credential_hint}>{asrCredentialHint}</span>
              </div>
              <input
                aria-label="ASR API Key"
                autoComplete="off"
                disabled={busy || asr.clear_credential}
                onChange={(event) => setAsr({ ...asr, credential: event.currentTarget.value })}
                placeholder={config?.asr.credential_configured ? "输入新的 API Key（留空保留现有）" : "请输入 API Key"}
                type="password"
                value={asr.credential}
              />
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
              <SelectionPopover<SpeechProviderKind>
                aria_label="TTS 服务提供商"
                content_width="content"
                disabled={busy}
                on_open_change={setTtsProviderOpen}
                on_select={handleTtsProviderChange}
                open={ttsProviderOpen}
                options={PROVIDER_OPTIONS}
                selected={tts.provider}
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
              <span>接口地址（Endpoint）</span>
              <input
                aria-label="TTS 接口地址"
                disabled={busy}
                onChange={(event) => setTts({ ...tts, endpoint: event.currentTarget.value })}
                placeholder={defaultEndpoint(tts.provider)}
                value={tts.endpoint}
              />
            </label>

            <label className={styles.field}>
              <span>模型名称</span>
              <input
                aria-label="TTS 模型名称"
                disabled={busy}
                onChange={(event) => setTts({ ...tts, model: event.currentTarget.value })}
                placeholder={defaultTtsModel(tts.provider)}
                value={tts.model}
              />
            </label>

            <label className={styles.field}>
              <span>音色名称</span>
              <input
                aria-label="TTS 音色名称"
                disabled={busy}
                onChange={(event) => setTts({ ...tts, voice: event.currentTarget.value })}
                placeholder={defaultTtsVoice(tts.provider)}
                value={tts.voice}
              />
            </label>

            <div className={`${styles.field} ${styles.full_width}`}>
              <div className={styles.credential_row}>
                <span>API Key 凭据</span>
                <span className={styles.credential_hint}>{ttsCredentialHint}</span>
              </div>
              <input
                aria-label="TTS API Key"
                autoComplete="off"
                disabled={busy || tts.clear_credential}
                onChange={(event) => setTts({ ...tts, credential: event.currentTarget.value })}
                placeholder={config?.tts.credential_configured ? "输入新的 API Key（留空保留现有）" : "请输入 API Key"}
                type="password"
                value={tts.credential}
              />
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
          </div>
        </article>
      </div>

      <footer className={styles.footer_actions}>
        <span>保存后会原子写入 config.toml 并热加载 Host 语音引擎，供已开启语音中继的网关优先复用。</span>
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
