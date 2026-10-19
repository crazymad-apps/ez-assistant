import { useState } from "react";
import { observer } from "mobx-react-lite";
import { SelectionPopover } from "../../../components/SelectionPopover";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import { deviceIcon } from "../../../platform/deviceIcon";
import styles from "./index.module.scss";

export const DevicePicker = observer(function DevicePicker(props: Readonly<{
  device: string | null;
  aria_label: string;
  disabled?: boolean;
  on_select: (device: string | null) => void;
}>) {
  const root = useRootStore();
  const [open, setOpen] = useState(false);
  const target = props.device;
  const platform = target
    ? root.network.devices.find((device) => device.id === target)?.platform
    : root.local_connection.capabilities?.platform;
  const name = root.network.label(target);
  return <div className={styles.root}>
    <SelectionPopover aria_label={props.aria_label}
    trigger_class_name={styles.trigger}
    trigger_content={<><Icon name={deviceIcon(platform)} size={16} /><span className={styles.trigger_name} title={name}>{name}</span></>}
    open={open} on_open_change={(value) => { setOpen(value); if (value) void root.network.refresh(); }}
    disabled={props.disabled} selected={target ?? "local"}
    options={[{ value: "local", label: root.network.label(null), icon: <Icon name={deviceIcon(root.local_connection.capabilities?.platform)} size={16} /> },
      ...root.network.online_devices.map((device) => ({ value: device.id, label: device.name,
        icon: <Icon name={deviceIcon(device.platform)} size={16} /> })),
      ...(root.network.next_offset === null ? [] : [{ value: "more", label: root.network.loading ? "正在加载…" : "加载更多设备", disabled: root.network.loading }])]}
    on_select={(value) => { if (value === "more") { void root.network.refresh(true); return; } props.on_select(value === "local" ? null : value); setOpen(false); }} />
  </div>;
});
