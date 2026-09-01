import { useRef } from "react";
import { Dialog } from "../../../../components/Dialog";
import { Icon } from "../../../../components/Icon";
import type { ConversationInputSourceSnapshot } from "../../../../generated/assistant-protocol";
import styles from "./index.module.scss";

type DeviceSource = Extract<ConversationInputSourceSnapshot, { type: "device" }>;

type DeviceMessageSourceDialogProps = Readonly<{
  on_close: () => void;
  source: DeviceSource;
}>;

/** 展示消息接受时冻结的来源事实，不用设备当前状态覆盖历史输入语义。 */
export function DeviceMessageSourceDialog(props: DeviceMessageSourceDialogProps) {
  const close_button_ref = useRef<HTMLButtonElement>(null);
  return (
    <Dialog
      aria_label="消息来源详情"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      initial_focus_ref={close_button_ref}
      on_close={props.on_close}
    >
      <header>
        <div><Icon name="device" size={17} /><h2>消息来源</h2></div>
        <button aria-label="关闭消息来源详情" onClick={props.on_close} ref={close_button_ref} type="button">
          <Icon name="x" size={16} />
        </button>
      </header>
      <dl>
        <DetailRow label="终端" value={props.source.device_name} />
        <DetailRow label="设备 ID" value={props.source.device_id} />
        <DetailRow label="输入方式" value={modalityLabel(props.source.modality)} />
        <DetailRow label="回复偏好" value={outputPreferenceLabel(props.source.requested_output)} />
      </dl>
      <p>以上为提交本条消息时记录的来源信息。</p>
    </Dialog>
  );
}

function DetailRow(props: Readonly<{ label: string; value: string }>) {
  return <div><dt>{props.label}</dt><dd>{props.value}</dd></div>;
}

function modalityLabel(modality: DeviceSource["modality"]): string {
  return modality === "speech_transcript" ? "语音转写" : "文字输入";
}

function outputPreferenceLabel(preference: DeviceSource["requested_output"]): string {
  return {
    text: "文字",
    audio: "语音",
    text_and_audio: "文字和语音",
  }[preference];
}
