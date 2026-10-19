import { observer } from "mobx-react-lite";
import { DevicePicker as HostDevicePicker } from "../../runtime-access/DevicePicker";
import { useRootStore } from "../../../stores/RootStoreContext";

export const DevicePicker = observer(function DevicePicker({ disabled }: Readonly<{ disabled: boolean }>) {
  const root = useRootStore();
  if (!root.network.available) return null;
  return <HostDevicePicker aria_label="新会话设备" device={root.navigation.target_device}
    disabled={disabled} on_select={(device) => root.switchDraftDevice(device)} />;
});
