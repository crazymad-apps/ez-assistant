import { observer } from "mobx-react-lite";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../../components/DropdownMenu";
import { Icon } from "../../../components/Icon";
import type { SessionSummary } from "../../../generated/assistant-protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const OutputHostingMenu = observer(function OutputHostingMenu(props: Readonly<{
  session: SessionSummary;
}>) {
  const store = useRootStore();
  const gateway = store.device_gateway;
  const devices = gateway.snapshot?.devices.filter((device) => (
    device.lifecycle === "paired" && device.connection
  )) ?? [];
  const hosting = props.session.pc_output_hosting;
  const label = hosting ? `回复频道：${hosting.device_name}` : "选择回复频道";

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        aria-label={label}
        className={styles.hosting_trigger}
        data-active={Boolean(hosting)}
        disabled={gateway.pending_action === "hosting"}
        title={label}
      >
        <Icon name="channel" size={16} />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" aria-label="选择回复频道" className={styles.hosting_menu}>
        <DropdownMenuItem
          aria-current={!hosting ? "true" : undefined}
          onSelect={() => void gateway.setOutputHosting(null)}
        >
          <Icon name="desktop" size={16} />
          <strong>仅在 Desktop 显示</strong>
          {!hosting && <Icon className={styles.hosting_option_tail} name="check" size={14} />}
        </DropdownMenuItem>
        {devices.map((device) => (
          <DropdownMenuItem
            aria-current={hosting?.device_id === device.device_id ? "true" : undefined}
            key={device.device_id}
            onSelect={() => void gateway.setOutputHosting(device.device_id)}
          >
            <Icon name="device" size={16} />
            <strong>{device.display_name}</strong>
            {hosting?.device_id === device.device_id && (
              <Icon className={styles.hosting_option_tail} name="check" size={14} />
            )}
          </DropdownMenuItem>
        ))}
        <DropdownMenuItem onSelect={() => store.settings.open("devices")}>
          <strong>管理智能终端</strong>
          <Icon className={styles.hosting_option_tail} name="chevron-right" size={14} />
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
});
