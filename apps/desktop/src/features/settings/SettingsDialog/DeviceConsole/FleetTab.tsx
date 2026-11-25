import { observer } from "mobx-react-lite";
import { useCallback, useEffect, useState } from "react";
import { SelectionPopover, type SelectionOption } from "../../../../components/SelectionPopover";
import { useRootStore } from "../../../../stores/RootStoreContext";
import {
  extractMediaRelativePath,
  normalizeConsolePath,
  parseFleetDevices,
  pickString,
  type FleetDeviceItem,
} from "./api";
import styles from "./index.module.scss";

type FleetTabProps = Readonly<{
  device_id: string;
  api_prefix: string;
  mode?: "proxy" | "native";
}>;

type CapabilityPreset = Readonly<{
  value: string;
  label: string;
  action: string;
  params: string;
}>;

const PROXY_CAPABILITY_PRESETS: ReadonlyArray<CapabilityPreset> = [
  { value: "camera.capture", label: "camera.capture (拍照)", action: "capture", params: "{}" },
  { value: "audio.volume", label: "audio.volume (音量)", action: "set_volume", params: '{"level":70}' },
  { value: "gpio.set", label: "gpio.set (GPIO)", action: "set", params: '{"pin":2,"level":1}' },
  { value: "sensor.read", label: "sensor.read (传感器)", action: "battery", params: '{"sensor_type":"battery"}' },
  { value: "lcd.display", label: "lcd.display (屏幕显示)", action: "show", params: '{"emotion":"happy","text":"你好"}' },
];

const NATIVE_CAPABILITY_PRESETS: ReadonlyArray<CapabilityPreset> = [
  {
    value: "device.display_show",
    label: "device.display_show (屏幕表情与文字)",
    action: "show",
    params: '{"emotion":"happy","text":"你好，我是 Mosaico"}',
  },
  {
    value: "device.audio_set_volume",
    label: "device.audio_set_volume (扬声器音量 0-100)",
    action: "set_volume",
    params: '{"level":70}',
  },
  {
    value: "device.sensor_read",
    label: "device.sensor_read (电量 / 温度 / IMU)",
    action: "battery",
    params: '{"sensor_type":"battery"}',
  },
  {
    value: "device.status_read",
    label: "device.status_read (设备实时遥测)",
    action: "read",
    params: "{}",
  },
  {
    value: "device.actuator_set",
    label: "device.actuator_set (GPIO 执行器)",
    action: "set",
    params: '{"pin":2,"level":1}',
  },
  {
    value: "device.voiceprint_list",
    label: "device.voiceprint_list (查询端侧声纹)",
    action: "list",
    params: "{}",
  },
];

export const FleetTab = observer(function FleetTab(props: FleetTabProps) {
  const { device_gateway: gateway } = useRootStore();
  const isNative = props.mode === "native";
  const presets = isNative ? NATIVE_CAPABILITY_PRESETS : PROXY_CAPABILITY_PRESETS;
  const capabilityOptions: ReadonlyArray<SelectionOption<string>> = presets.map((item) => ({
    value: item.value,
    label: item.label,
  }));
  const [devices, setDevices] = useState<FleetDeviceItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [targetDevice, setTargetDevice] = useState("");
  const [capability, setCapability] = useState(presets[0].value);
  const [action, setAction] = useState(presets[0].action);
  const [paramsJson, setParamsJson] = useState(presets[0].params);
  const [cmdOpen, setCmdOpen] = useState(false);
  const [capOpen, setCapOpen] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [cmdOutput, setCmdOutput] = useState<string | null>(null);
  const [mediaObjectUrl, setMediaObjectUrl] = useState<string | null>(null);

  useEffect(() => {
    return () => {
      if (mediaObjectUrl) URL.revokeObjectURL(mediaObjectUrl);
    };
  }, [mediaObjectUrl]);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const payload = await gateway.consoleJson<unknown>(
        props.device_id,
        normalizeConsolePath(props.api_prefix, "devices"),
      );
      const parsed = parseFleetDevices(payload);
      setDevices(parsed);
      setTargetDevice((prev) => {
        if (prev.trim()) return prev;
        const first = parsed[0]?.device_id;
        return first && first !== "-" ? first : "";
      });
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "加载子设备列表失败。");
    } finally {
      setLoading(false);
    }
  }, [gateway, props.api_prefix, props.device_id]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function submitCommand(event: React.FormEvent) {
    event.preventDefault();
    const target = targetDevice.trim();
    if (!target) {
      setError("请选择或输入目标终端 MAC。");
      return;
    }
    setSubmitting(true);
    setError(null);
    setMediaObjectUrl(null);
    try {
      const res = await gateway.consoleJson<Record<string, unknown>>(
        props.device_id,
        normalizeConsolePath(props.api_prefix, `devices/${encodeURIComponent(target)}/commands`),
        {
          method: "POST",
          body: {
            agent_id: "ez-assistant",
            capability,
            action: action.trim() || "capture",
            params_json: paramsJson.trim() || "{}",
            timeout_ms: 5000,
          },
        },
      );
      setCmdOutput(JSON.stringify(res, null, 2));
      const mediaUrl = pickString(res, "mediaUrl", "media_url");
      const mediaPath = extractMediaRelativePath(mediaUrl);
      if (mediaPath) {
        const blob = await gateway.consoleBlob(props.device_id, mediaPath);
        setMediaObjectUrl(URL.createObjectURL(blob));
      }
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "指令执行失败。");
    } finally {
      setSubmitting(false);
    }
  }

  const deviceOptions: SelectionOption<string>[] = devices
    .filter((d) => d.device_id && d.device_id !== "-")
    .map((d) => ({ value: d.device_id, label: `${d.zone} (${d.device_id})` }));

  return (
    <div className={styles.section_grid}>
      <section className={styles.panel}>
        <header className={styles.panel_header}>
          <h4>{isNative ? `直连硬件状态与实时遥测 (${devices.length})` : `在线与已登记子终端 (${devices.length})`}</h4>
          <button disabled={loading} onClick={() => void refresh()} type="button">
            {loading ? "刷新中…" : isNative ? "刷新状态" : "刷新拓扑"}
          </button>
        </header>
        <div className={styles.panel_body}>
          {error ? <p className={styles.error_banner} role="alert">{error}</p> : null}
          {devices.length > 0 ? (
            <div className={styles.card_grid}>
              {devices.map((item) => (
                <article className={styles.fleet_card} key={item.device_id}>
                  <div className={styles.fleet_card_top}>
                    <strong>{item.zone} ({item.device_id})</strong>
                    <span className={styles.badge} data-tone={item.online ? "online" : "offline"}>
                      {item.online ? "ONLINE" : "OFFLINE"}
                    </span>
                  </div>
                  <div className={styles.meta_line}>
                    group={item.group_id} · chip={item.chip} · vp={item.vp_version} · vad={item.vad_state}
                  </div>
                  <div className={styles.meta_line}>
                    {[
                      `rssi=${item.rssi}`,
                      `vol=${item.volume}`,
                      item.battery_pct ? `bat=${item.battery_pct}` : null,
                      item.temperature_c ? `temp=${item.temperature_c}` : null,
                      item.free_int ? `ram=${item.free_int}` : null,
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                  </div>
                  <div className={styles.cap_row}>
                    {item.capabilities.length > 0 ? (
                      item.capabilities.map((cap) => (
                        <span className={styles.cap_tag} key={cap}>{cap}</span>
                      ))
                    ) : (
                      <span className={styles.cap_tag}>无声明能力</span>
                    )}
                  </div>
                </article>
              ))}
            </div>
          ) : (
            <p className={styles.empty_note}>
              {isNative
                ? "暂未获取到直连硬件状态，请点击刷新状态。"
                : "暂无终端接入记录，请先确认 MAC 白名单并上电终端。"}
            </p>
          )}
        </div>
      </section>

      <section className={styles.panel}>
        <header className={styles.panel_header}>
          <h4>{isNative ? "下发硬件控制指令（经 WSS + ACL 校验）" : "向子终端下发控制指令"}</h4>
        </header>
        <form className={styles.panel_body} onSubmit={(event) => void submitCommand(event)}>
          <div className={styles.form_grid}>
            {deviceOptions.length > 0 ? (
              <div className={styles.field}>
                <span>快速选择终端</span>
                <SelectionPopover
                  aria_label="选择子终端"
                  content_width="content"
                  on_open_change={setCmdOpen}
                  on_select={(val) => {
                    setTargetDevice(val);
                    setCmdOpen(false);
                  }}
                  open={cmdOpen}
                  options={deviceOptions}
                  selected={targetDevice}
                  trigger_variant="field"
                />
              </div>
            ) : null}

            <label className={styles.field}>
              <span>目标终端 MAC</span>
              <input
                aria-label="目标终端 MAC"
                onChange={(event) => setTargetDevice(event.currentTarget.value)}
                placeholder="30:ed:a0:f3:dc:70"
                value={targetDevice}
              />
            </label>

            <div className={styles.field}>
              <span>能力 (Capability)</span>
              <SelectionPopover
                aria_label="能力选择"
                content_width="content"
                on_open_change={setCapOpen}
                on_select={(val) => {
                  setCapability(val);
                  const preset = presets.find((item) => item.value === val);
                  if (preset) {
                    setAction(preset.action);
                    setParamsJson(preset.params);
                  }
                  setCapOpen(false);
                }}
                open={capOpen}
                options={capabilityOptions}
                selected={capability}
                trigger_variant="field"
              />
            </div>

            <label className={styles.field}>
              <span>动作 (Action)</span>
              <input
                aria-label="动作"
                onChange={(event) => setAction(event.currentTarget.value)}
                placeholder="capture"
                value={action}
              />
            </label>

            <label className={`${styles.field} ${styles.full_span}`}>
              <span>参数 JSON</span>
              <textarea
                aria-label="参数 JSON"
                onChange={(event) => setParamsJson(event.currentTarget.value)}
                value={paramsJson}
              />
            </label>
          </div>

          <div className={styles.action_row}>
            <button className={styles.primary_btn} disabled={submitting} type="submit">
              {submitting ? "执行中…" : "发送指令"}
            </button>
          </div>

          {cmdOutput ? <pre className={styles.code_box}>{cmdOutput}</pre> : null}
          {mediaObjectUrl ? (
            <img alt="终端抓拍画面" className={styles.snapshot_preview} src={mediaObjectUrl} />
          ) : null}
        </form>
      </section>
    </div>
  );
});
