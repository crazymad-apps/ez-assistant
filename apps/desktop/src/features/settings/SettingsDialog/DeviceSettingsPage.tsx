import { observer } from "mobx-react-lite";
import { useEffect, useState } from "react";
import { Icon } from "../../../components/Icon";
import { PresenceBoundary } from "../../../components/Presence";
import type {
  DeviceCapabilitiesSnapshot,
  DeviceSummarySnapshot,
  DiscoveredDeviceSnapshot,
  PendingDevicePairingSnapshot,
} from "@ez-assistant/protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { SessionActionDialog } from "../../sessions/SessionActionDialog";
import { DeviceDetailPage, type ConsoleTabKey } from "./DeviceDetailPage";
import { WakeSettingsCard } from "./DeviceConsole/WakeSettingsCard";
import { DeviceSpeechSection } from "./DeviceSpeechSection";
import { SettingsPageContainer } from "./SettingsPageContainer";
import pageStyles from "./DeviceSettingsPage.module.scss";
import styles from "./index.module.scss";

export const DeviceSettingsPage = observer(function DeviceSettingsPage() {
  const store = useRootStore();
  const gateway = store.device_gateway;
  const snapshot = gateway.snapshot;
  const [editing_device, setEditingDevice] = useState<string | null>(null);
  const [editing_name, setEditingName] = useState("");
  const [revoking_device, setRevokingDevice] = useState<DeviceSummarySnapshot | null>(null);
  const [detail_device_id, setDetailDeviceId] = useState<string | null>(null);
  const [detail_tab, setDetailTab] = useState<ConsoleTabKey>("overview");

  useEffect(() => {
    void gateway.load();
    void gateway.loadSpeechConfiguration();
  }, [gateway]);

  const paired_devices = snapshot?.devices.filter((device) => device.lifecycle === "paired") ?? [];
  const discovered_devices = snapshot?.discovered_devices ?? [];
  const discovered_fingerprints = new Set(
    discovered_devices.map((device) => device.key_fingerprint.toUpperCase()),
  );
  const direct_pending_pairings = (snapshot?.pending_pairings ?? []).filter(
    (pending) =>
      pending.mode === "direct_confirm"
      && (!pending.key_fingerprint
        || !discovered_fingerprints.has(pending.key_fingerprint.toUpperCase())),
  );
  const nearby_count = discovered_devices.length + direct_pending_pairings.length;
  const detail_device = paired_devices.find((device) => device.device_id === detail_device_id) ?? null;
  const online_native_devices = paired_devices.filter(
    (device) => device.connection?.console?.mode === "native",
  );

  function openDeviceDetail(device_id: string, tab: ConsoleTabKey = "overview") {
    setDetailTab(tab);
    setDetailDeviceId(device_id);
  }

  if (detail_device) {
    return (
      <DeviceDetailPage
        device={detail_device}
        initial_tab={detail_tab}
        on_back={() => {
          setDetailDeviceId(null);
          setDetailTab("overview");
        }}
      />
    );
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
        <button
          disabled={gateway.loading || gateway.pending_action !== null}
          onClick={() => {
            void gateway.load();
            void gateway.loadSpeechConfiguration();
          }}
          type="button"
        >
          <Icon name="refresh" size={14} />
          刷新
        </button>
      )}
      title="智能终端"
    >
      <article className={styles.device_gateway_card}>
        <div>
          <span className={styles.device_icon}><Icon name="device" size={19} /></span>
          <div className={pageStyles.gateway_title_group}>
            <strong>设备接入</strong>
            <small>
              {snapshot?.enabled
                ? "已开启局域网自动发现，Mosaico 开机连上同一 Wi-Fi 即可连接"
                : "关闭后已配对终端将暂停自动重连"}
            </small>
          </div>
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
                    <input placeholder="请输入"
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
                      onClick={() => openDeviceDetail(device.device_id, "overview")}
                      type="button"
                    >
                      {device.display_name}
                    </button>
                  ) : null}
                </div>
                <div className={styles.paired_device_actions}>
                  {editing_device !== device.device_id ? (
                    <button
                      disabled={gateway.pending_action !== null}
                      onClick={() => openDeviceDetail(device.device_id, "overview")}
                      type="button"
                    >
                      管理
                    </button>
                  ) : null}
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
        ) : <p className={styles.device_empty}>尚未配对 Mosaico 终端。</p>}
      </section>

      <section className={styles.device_section}>
        <header>
          <h4>附近可连接的设备</h4>
          <span className={styles.device_count}>{nearby_count}</span>
        </header>
        {nearby_count > 0 ? (
          <>
            {discovered_devices.length > 0 ? (
              <div className={styles.discovered_device_list}>
                {discovered_devices.map((device) => (
                  <DiscoveredDeviceRow
                    device={device}
                    gateway={gateway}
                    key={device.discovery_id}
                  />
                ))}
              </div>
            ) : null}
            {direct_pending_pairings.length > 0 ? (
              <div className={styles.pending_device_list}>
                {direct_pending_pairings.map((pending) => (
                  <DirectConfirmPairingRow
                    gateway={gateway}
                    key={pending.pairing_request_id}
                    pending={pending}
                  />
                ))}
              </div>
            ) : null}
          </>
        ) : paired_devices.length > 0 ? (
          <p className={pageStyles.nearby_empty_compact}>
            未发现新的 Mosaico 终端，新设备开机连上同一 Wi-Fi 后会自动出现。
          </p>
        ) : (
          <p className={styles.device_empty}>Mosaico 终端开机连上同一 Wi-Fi 后会自动出现在这里。</p>
        )}
      </section>

      {online_native_devices.map((device) => (
        <WakeSettingsCard
          api_prefix={device.connection?.console?.api_prefix ?? "/api/v1"}
          device_id={device.device_id}
          device_name={device.display_name}
          key={`wake-${device.device_id}`}
          on_open_voiceprint={() => openDeviceDetail(device.device_id, "voice")}
          standalone_spacing
        />
      ))}

      <DeviceSpeechSection />

      {gateway.error_message && <p className={styles.error_message} role="alert">{gateway.error_message}</p>}
      {gateway.notice_message && <p className={styles.notice_message} role="status">{gateway.notice_message}</p>}

      <PresenceBoundary present={revoking_device !== null}>
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
      </PresenceBoundary>
    </SettingsPageContainer>
  );
});

const DirectConfirmPairingRow = observer(function DirectConfirmPairingRow(props: Readonly<{
  gateway: ReturnType<typeof useRootStore>["device_gateway"];
  pending: PendingDevicePairingSnapshot;
}>) {
  const pending_action = props.gateway.pending_action === `pairing:${props.pending.pairing_request_id}`;
  const subtitle = [
    capabilityLabel(props.pending.capabilities),
    props.pending.peer_address,
    props.pending.key_fingerprint ? `指纹 ${props.pending.key_fingerprint}` : null,
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <article data-mode="direct_confirm">
      <span className={styles.device_icon}><Icon name="device" size={18} /></span>
      <div>
        <strong>{props.pending.display_name}</strong>
        <small>{subtitle}</small>
      </div>
      <div className={styles.pending_device_actions}>
        <button
          className={styles.pending_device_confirm}
          disabled={pending_action}
          onClick={() => void props.gateway.confirmPairing(props.pending.pairing_request_id, null, null)}
          type="button"
        >
          {pending_action ? "配对中…" : "确认配对"}
        </button>
        <button
          className={styles.pending_device_reject}
          disabled={pending_action}
          onClick={() => void props.gateway.rejectPairing(props.pending.pairing_request_id)}
          type="button"
        >
          拒绝
        </button>
      </div>
    </article>
  );
});

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

const DiscoveredDeviceRow = observer(function DiscoveredDeviceRow(props: Readonly<{
  gateway: ReturnType<typeof useRootStore>["device_gateway"];
  device: DiscoveredDeviceSnapshot;
}>) {
  const connecting = props.gateway.pending_action === `connect:${props.device.discovery_id}`;
  const busy = props.gateway.pending_action !== null || props.gateway.stale;
  const subtitle = [
    props.device.model,
    props.device.peer_address,
    `指纹 ${props.device.key_fingerprint}`,
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <article>
      <span className={styles.device_icon}><Icon name="device" size={18} /></span>
      <div>
        <strong>{props.device.display_name}</strong>
        <small>{subtitle}</small>
      </div>
      <button
        className={styles.pending_device_confirm}
        disabled={busy}
        onClick={() => void props.gateway.connectDiscoveredDevice(props.device.discovery_id)}
        type="button"
      >
        {connecting ? "连接中…" : "连接"}
      </button>
    </article>
  );
});
