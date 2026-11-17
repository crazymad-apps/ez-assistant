import { DESKTOP_PINS } from "../../../stores/SidebarOrderStore";
import { desktopPinKey } from "../../../stores/DesktopPinsStore";
import { observer } from "mobx-react-lite";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const DesktopPins = observer(function DesktopPins() {
  const store = useRootStore();
  return <div className={styles.session_list} data-indent="root">
    {store.pins.error && <div role="alert">{store.pins.error}{!store.pins.value.imported && <button type="button" disabled={store.pins.busy} onClick={() => void store.retryPinImport()}>重试导入</button>}</div>}
    {store.sidebar_order.rows(DESKTOP_PINS, store.pins.value.items, desktopPinKey).map((pin) => {
      const projection = pin.device ? store.network.contexts.get(pin.device)?.projection : store.local_projection;
      const execution = pin.device ? store.network.contexts.get(pin.device)?.live_execution : store.local_live_execution;
      const current = projection?.sessionSummary(pin.session_id);
      const title = current?.title ?? pin.title;
      const device = pin.device ? store.network.devices.find((item) => item.id === pin.device) : null;
      const name = device?.name ?? pin.device_name;
      const hint = pin.device && !device?.online ? " · 离线或暂不可访问" : "";
      return <div className={styles.session_row} data-sidebar-scope={JSON.stringify(DESKTOP_PINS)} data-sidebar-item={desktopPinKey(pin)} aria-busy={store.sidebar_order.isPending(DESKTOP_PINS)} data-variant="pinned" data-has-actions="true" data-selected={store.navigation.target_device === pin.device && store.navigation.selected_session_id === pin.session_id} key={JSON.stringify([pin.device, pin.session_id])}>
        <button className={styles.session_open} data-navigation-action data-sidebar-handle type="button" title={`${title} · ${name}${hint}`} onClick={() => void store.selectSession(pin.session_id, pin.device)}>
          {pin.device && <Icon name="desktop" size={14} />}
          <span className={styles.session_title}>{title}</span>
        </button>
        <button className={styles.session_pin_action} type="button" aria-label={`取消置顶 ${title}`} disabled={store.pins.busy} onClick={() => void store.removePin(pin)}><Icon name="pin" size={14} /></button>
        {execution?.activeRunId(pin.session_id, current?.active_run_id) && (
          <svg
            aria-label="正在运行"
            className={styles.loading_ring}
            role="img"
            viewBox="0 0 16 16"
          >
            <circle className={styles.loading_ring_track} cx="8" cy="8" r="6" />
            <circle className={styles.loading_ring_arc} cx="8" cy="8" r="6" />
          </svg>
        )}
      </div>;
    })}
  </div>;
});
