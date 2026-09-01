import { observer } from "mobx-react-lite";
import { useEffect, useState } from "react";
import { Icon } from "../../../components/Icon";
import type {
  DeviceCapabilitiesSnapshot,
  DeviceSummarySnapshot,
  PendingDevicePairingSnapshot,
  SpeechServiceStatusSnapshot,
} from "../../../generated/assistant-protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { SessionActionDialog } from "../../sessions/SessionActionDialog";
import { DeviceDetailPage } from "./DeviceDetailPage";
import { SettingsPageContainer } from "./SettingsPageContainer";
import styles from "./index.module.scss";

export const DeviceSettingsPage = observer(function DeviceSettingsPage() {
  const store = useRootStore();
  const gateway = store.device_gateway;
  const snapshot = gateway.snapshot;
  const [editing_device, setEditingDevice] = useState<string | null>(null);
  const [editing_name, setEditingName] = useState("");
  const [revoking_device, setRevokingDevice] = useState<DeviceSummarySnapshot | null>(null);
  const [detail_device_id, setDetailDeviceId] = useState<string | null>(null);

  useEffect(() => {
    void gateway.load();
  }, [gateway]);

  const paired_devices = snapshot?.devices.filter((device) => device.lifecycle === "paired") ?? [];
  const detail_device = paired_devices.find((device) => device.device_id === detail_device_id) ?? null;

  if (detail_device) {
    return <DeviceDetailPage device={detail_device} on_back={() => setDetailDeviceId(null)} />;
  }

  async function saveName(device: DeviceSummarySnapshot) {
    if (await gateway.renameDevice(device.device_id, editing_name)) {
      setEditingDevice(null);
    }
  }

  async function revokeDevice() {
    if (revoking_device && await gateway.revokeDevice(revoking_device.device_id)) {
      setRevokingDevice(null);
    }
  }

  return (
    <SettingsPageContainer
      actions={(
        <button disabled={gateway.loading || gateway.pending_action !== null} onClick={() => void gateway.load()} type="button">
          <Icon name="refresh" size={14} />
          刷新
        </button>
      )}
      title="智能终端"
    >
      <article className={styles.device_gateway_card}>
        <div>
          <span className={styles.device_icon}><Icon name="device" size={19} /></span>
          <strong>设备接入</strong>
        </div>
        <button
          aria-checked={snapshot?.enabled ?? false}
          aria-label="智能终端接入"
          className={styles.device_access_switch}
          disabled={gateway.stale || gateway.pending_action !== null}
          onClick={() => void gateway.setAccessEnabled(!(snapshot?.enabled ?? false))}
          role="switch"
          type="button"
        >
          <i />
        </button>
      </article>

      <section className={styles.device_section}>
        <header>
          <h4>添加设备</h4>
          {snapshot?.pairing_window ? (
            <button
              disabled={gateway.pending_action !== null}
              onClick={() => void gateway.closePairingWindow()}
              type="button"
            >
              结束添加
            </button>
          ) : (
            <button
              disabled={!snapshot?.enabled || !snapshot.available || gateway.pending_action !== null}
              onClick={() => void gateway.openPairingWindow()}
              type="button"
            >
              添加设备
            </button>
          )}
        </header>
        {snapshot?.pairing_window ? (
          snapshot.pending_pairings.length > 0 ? (
            <div className={styles.pending_device_list}>
              {snapshot.pending_pairings.map((pending) => (
                <PendingPairing gateway={gateway} key={pending.pairing_request_id} pending={pending} />
              ))}
            </div>
          ) : <p className={styles.device_empty}>正在等待终端发起配对…</p>
        ) : <p className={styles.device_empty}>点击“添加设备”后，附近未配对终端才能申请接入。</p>}
      </section>

      <section className={styles.device_section}>
        <header>
          <h4>已配对设备</h4>
          <span className={styles.device_count}>{paired_devices.length}</span>
        </header>
        {paired_devices.length > 0 ? (
          <div className={styles.paired_device_list}>
            {paired_devices.map((device) => (
              <article key={device.device_id}>
                <i
                  aria-label={device.connection ? "在线" : "离线"}
                  data-online={Boolean(device.connection)}
                  role="img"
                />
                <div className={styles.paired_device_summary}>
                  {editing_device === device.device_id ? (
                    <input
                      aria-label={`设备名称 ${device.display_name}`}
                      autoFocus
                      maxLength={80}
                      onChange={(event) => setEditingName(event.currentTarget.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter") void saveName(device);
                        if (event.key === "Escape") setEditingDevice(null);
                      }}
                      value={editing_name}
                    />
                  ) : null}
                  {editing_device !== device.device_id ? (
                    <button
                      aria-label={`查看 ${device.display_name} 详情`}
                      className={styles.paired_device_name_button}
                      onClick={() => setDetailDeviceId(device.device_id)}
                      type="button"
                    >
                      {device.display_name}
                    </button>
                  ) : null}
                </div>
                <div className={styles.paired_device_actions}>
                  {editing_device === device.device_id ? (
                    <>
                      <button disabled={gateway.pending_action !== null} onClick={() => setEditingDevice(null)} type="button">取消</button>
                      <button disabled={gateway.pending_action !== null} onClick={() => void saveName(device)} type="button">保存</button>
                    </>
                  ) : (
                    <button
                      disabled={gateway.pending_action !== null}
                      onClick={() => {
                        setEditingDevice(device.device_id);
                        setEditingName(device.display_name);
                      }}
                      type="button"
                    >
                      重命名
                    </button>
                  )}
                  <button
                    className={styles.device_danger_button}
                    disabled={gateway.pending_action !== null}
                    onClick={() => setRevokingDevice(device)}
                    type="button"
                  >
                    解除配对
                  </button>
                </div>
              </article>
            ))}
          </div>
        ) : <p className={styles.device_empty}>尚未配对智能终端。</p>}
      </section>

      <section className={styles.device_section}>
        <header>
          <h4>语音服务</h4>
        </header>
        <dl className={styles.speech_status_list}>
          <div><dt>语音识别</dt><dd data-status={snapshot?.speech_services.asr}>{speechStatusLabel(snapshot?.speech_services.asr)}</dd></div>
          <div><dt>语音播放</dt><dd data-status={snapshot?.speech_services.tts}>{speechStatusLabel(snapshot?.speech_services.tts)}</dd></div>
        </dl>
      </section>

      {gateway.error_message && <p className={styles.error_message} role="alert">{gateway.error_message}</p>}
      {gateway.notice_message && <p className={styles.notice_message} role="status">{gateway.notice_message}</p>}

      {revoking_device && (
        <SessionActionDialog
          confirm_label="解除配对"
          is_danger
          is_pending={gateway.pending_action === `revoke:${revoking_device.device_id}`}
          on_cancel={() => setRevokingDevice(null)}
          on_confirm={() => void revokeDevice()}
          title="解除这个设备的配对？"
        >
          <p><strong>{revoking_device.display_name}</strong> 将不能继续连接、提交输入或接收回复。</p>
          <p>如果它是 PC 输出托管目标，托管会同时解除；已有 Conversation 不受影响。</p>
        </SessionActionDialog>
      )}
    </SettingsPageContainer>
  );
});

const PendingPairing = observer(function PendingPairing(props: Readonly<{
  gateway: ReturnType<typeof useRootStore>["device_gateway"];
  pending: PendingDevicePairingSnapshot;
}>) {
  const [code, setCode] = useState("");
  const pending_action = props.gateway.pending_action === `pairing:${props.pending.pairing_request_id}`;
  return (
    <article>
      <span className={styles.device_icon}><Icon name="device" size={18} /></span>
      <div>
        <strong>{props.pending.display_name}</strong>
        <small>{capabilityLabel(props.pending.capabilities)} · 还可尝试 {props.pending.remaining_attempts} 次</small>
      </div>
      <label>
        <input
          aria-label={`${props.pending.display_name} 配对码`}
          autoComplete="one-time-code"
          disabled={pending_action}
          inputMode="numeric"
          maxLength={6}
          onChange={(event) => setCode(event.currentTarget.value.replace(/\D/g, "").slice(0, 6))}
          placeholder="配对码"
          value={code}
        />
      </label>
      <button
        disabled={pending_action || code.length !== 6}
        onClick={() => void props.gateway.confirmPairing(props.pending.pairing_request_id, code, null)}
        type="button"
      >
        {pending_action ? "配对中…" : "确认配对"}
      </button>
    </article>
  );
});

function speechStatusLabel(status?: SpeechServiceStatusSnapshot): string {
  return ({ ready: "可用", degraded: "部分可用", unavailable: "不可用" } as Record<string, string>)[status ?? ""] ?? "待同步";
}

function capabilityLabel(capabilities?: DeviceCapabilitiesSnapshot): string {
  if (!capabilities) return "能力将在设备在线后同步";
  const values = [
    capabilities.input_text ? "文字输入" : null,
    capabilities.input_pcm16_16k_mono ? "语音输入" : null,
    capabilities.output_text ? "文字输出" : null,
    capabilities.output_pcm16_16k_mono ? "语音播放" : null,
  ].filter(Boolean);
  return values.length > 0 ? values.join(" · ") : "未声明交互能力";
}
