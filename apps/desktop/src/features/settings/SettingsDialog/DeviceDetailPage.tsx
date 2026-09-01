import type { DeviceCapabilitiesSnapshot, DeviceSummarySnapshot } from "../../../generated/assistant-protocol";
import { SettingsPageContainer } from "./SettingsPageContainer";
import styles from "./index.module.scss";

type DeviceDetailPageProps = Readonly<{
  device: DeviceSummarySnapshot;
  on_back: () => void;
}>;

/** 详情页只展开当前 Gateway 快照，不能把易失连接信息复制成可写的前端设备状态。 */
export function DeviceDetailPage(props: DeviceDetailPageProps) {
  const connection = props.device.connection;
  return (
    <SettingsPageContainer
      back_label="返回设备列表"
      on_back={props.on_back}
      title={props.device.display_name}
    >
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
      </dl>
      {!connection ? <p className={styles.device_detail_note}>设备重新在线后，会显示本次连接协商出的能力与输出偏好。</p> : null}
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
