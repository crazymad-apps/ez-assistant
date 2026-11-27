import { useState } from "react";
import type { DeviceCapabilitiesSnapshot, DeviceSummarySnapshot } from "@ez-assistant/protocol";
import { AclMatrixTab } from "./DeviceConsole/AclMatrixTab";
import { FleetTab } from "./DeviceConsole/FleetTab";
import { MacWhitelistTab } from "./DeviceConsole/MacWhitelistTab";
import { VoiceprintTab } from "./DeviceConsole/VoiceprintTab";
import consoleStyles from "./DeviceConsole/index.module.scss";
import { SettingsPageContainer } from "./SettingsPageContainer";
import styles from "./index.module.scss";

export type ConsoleTabKey = "overview" | "fleet" | "macs" | "acls" | "voice";

type DeviceDetailPageProps = Readonly<{
  device: DeviceSummarySnapshot;
  initial_tab?: ConsoleTabKey;
  on_back: () => void;
}>;

const PROXY_CONSOLE_TABS: ReadonlyArray<Readonly<{ key: ConsoleTabKey; label: string }>> = [
  { key: "overview", label: "概览" },
  { key: "fleet", label: "设备拓扑与指令" },
  { key: "macs", label: "MAC 白名单" },
  { key: "acls", label: "ACL 权限矩阵" },
  { key: "voice", label: "声纹与语音" },
];

const NATIVE_CONSOLE_TABS: ReadonlyArray<Readonly<{ key: ConsoleTabKey; label: string }>> = [
  { key: "overview", label: "概览" },
  { key: "fleet", label: "设备控制与状态" },
  { key: "acls", label: "ACL 权限矩阵" },
  { key: "voice", label: "声纹与语音" },
];

/** 详情页只展开当前 Gateway 快照与受控控制台视图，不在前端持久化易失连接态。 */
export function DeviceDetailPage(props: DeviceDetailPageProps) {
  const connection = props.device.connection;
  const consoleMeta = connection?.console ?? null;
  const isNativeConsole = consoleMeta?.mode === "native";
  const consoleTabs = isNativeConsole ? NATIVE_CONSOLE_TABS : PROXY_CONSOLE_TABS;
  const [activeTab, setActiveTab] = useState<ConsoleTabKey>(props.initial_tab ?? "overview");
  const [copiedDirectUrl, setCopiedDirectUrl] = useState(false);

  const currentTab: ConsoleTabKey =
    !consoleMeta || (isNativeConsole && activeTab === "macs") ? "overview" : activeTab;

  async function copyDirectUrl(url: string) {
    try {
      await navigator.clipboard?.writeText(url);
      setCopiedDirectUrl(true);
      window.setTimeout(() => setCopiedDirectUrl(false), 1500);
    } catch {
      setCopiedDirectUrl(false);
    }
  }

  return (
    <SettingsPageContainer
      back_label="返回设备列表"
      on_back={props.on_back}
      title={props.device.display_name}
    >
      {consoleMeta ? (
        <div aria-label="终端管理页签" className={consoleStyles.tabs} role="tablist">
          {consoleTabs.map((tab) => (
            <button
              aria-selected={currentTab === tab.key}
              key={tab.key}
              onClick={() => setActiveTab(tab.key)}
              role="tab"
              type="button"
            >
              {tab.label}
            </button>
          ))}
        </div>
      ) : null}

      {currentTab === "overview" ? (
        <>
          <dl className={styles.device_detail_list}>
            <DetailRow label="状态" value={connection ? "在线" : "离线"} value_state={connection ? "online" : "offline"} />
            <DetailRow label="设备 ID" value={props.device.device_id} />
            <DetailRow label="配对时间" value={formatTime(props.device.paired_at_ms)} />
            <DetailRow label="身份更新时间" value={formatTime(props.device.updated_at_ms)} />
            {connection ? <DetailRow label="本次连接" value={formatTime(connection.connected_at_ms)} /> : null}
            {connection ? <DetailRow label="输出偏好" value={preferenceLabel(connection.output_preference)} /> : null}
            {connection ? <DetailRow label="输入能力" value={inputCapabilities(connection.capabilities)} /> : null}
            {connection ? <DetailRow label="输出能力" value={outputCapabilities(connection.capabilities)} /> : null}
            {connection ? <DetailRow label="交互能力" value={interactionCapabilities(connection.capabilities)} /> : null}
            {connection ? (
              <DetailRow label="语音中继" value={connection.speech_relay ? "已开启" : "未开启"} />
            ) : null}
            {connection?.peer_address ? <DetailRow label="连接地址" value={connection.peer_address} /> : null}
            {connection?.client_version ? <DetailRow label="客户端版本" value={connection.client_version} /> : null}
            {connection && connection.mcp_server_keys && connection.mcp_server_keys.length > 0 ? (
              <DetailRow label="自动配置 MCP" value={connection.mcp_server_keys.join("、")} />
            ) : null}
            {consoleMeta?.direct_url ? (
              <div>
                <dt>原始控制台</dt>
                <dd className={consoleStyles.action_row} style={{ justifyContent: "flex-end" }}>
                  <code>{consoleMeta.direct_url}</code>
                  <button
                    className={consoleStyles.secondary_btn}
                    onClick={() => void copyDirectUrl(consoleMeta.direct_url!)}
                    type="button"
                  >
                    {copiedDirectUrl ? "已复制" : "复制地址"}
                  </button>
                </dd>
              </div>
            ) : null}
          </dl>
          {!connection ? (
            <p className={styles.device_detail_note}>设备重新在线后，会显示本次连接协商出的能力与输出偏好。</p>
          ) : null}
        </>
      ) : null}

      {currentTab === "fleet" && consoleMeta ? (
        <FleetTab
          api_prefix={consoleMeta.api_prefix}
          device_id={props.device.device_id}
          mode={consoleMeta.mode}
        />
      ) : null}

      {currentTab === "macs" && consoleMeta && !isNativeConsole ? (
        <MacWhitelistTab api_prefix={consoleMeta.api_prefix} device_id={props.device.device_id} />
      ) : null}

      {currentTab === "acls" && consoleMeta ? (
        <AclMatrixTab api_prefix={consoleMeta.api_prefix} device_id={props.device.device_id} />
      ) : null}

      {currentTab === "voice" && consoleMeta ? (
        <VoiceprintTab
          api_prefix={consoleMeta.api_prefix}
          device_id={props.device.device_id}
          device_name={props.device.display_name}
          mode={consoleMeta.mode}
        />
      ) : null}
    </SettingsPageContainer>
  );
}

function DetailRow(props: Readonly<{ label: string; value: string; value_state?: "online" | "offline" }>) {
  return <div><dt>{props.label}</dt><dd data-state={props.value_state}>{props.value}</dd></div>;
}

function formatTime(timestamp_ms: number): string {
  return new Intl.DateTimeFormat("zh-CN", {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(timestamp_ms));
}

function preferenceLabel(preference: string): string {
  return ({
    text: "文字",
    audio: "语音",
    text_and_audio: "文字和语音",
  } as Record<string, string>)[preference] ?? "未知";
}

function inputCapabilities(capabilities: DeviceCapabilitiesSnapshot): string {
  return enabledLabels([
    [capabilities.input_text, "文字"],
    [capabilities.input_pcm16_16k_mono, "PCM16 语音"],
  ]);
}

function outputCapabilities(capabilities: DeviceCapabilitiesSnapshot): string {
  return enabledLabels([
    [capabilities.output_text, "文字"],
    [capabilities.output_pcm16_16k_mono, "PCM16 语音"],
  ]);
}

function interactionCapabilities(capabilities: DeviceCapabilitiesSnapshot): string {
  return enabledLabels([
    [capabilities.display_status, "状态显示"],
    [capabilities.display_transcript, "转写显示"],
    [capabilities.playback_cancel, "取消播放"],
  ]);
}

function enabledLabels(values: ReadonlyArray<readonly [boolean, string]>): string {
  const enabled = values.filter(([available]) => available).map(([, label]) => label);
  return enabled.length > 0 ? enabled.join("、") : "无";
}
