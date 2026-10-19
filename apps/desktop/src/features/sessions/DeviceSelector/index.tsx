import { observer } from "mobx-react-lite";
import { DevicePicker } from "../../runtime-access/DevicePicker";
import { Button } from "../../../components/Button";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const DeviceSelector = observer(function DeviceSelector() {
  const store = useRootStore();
  const network = store.network;
  if (!network.available && !network.loading && !network.error) return null;
  const context = store.sidebar_context;
  return <section className={styles.selector} aria-label="浏览设备">
    <DevicePicker aria_label="切换浏览设备" device={network.browsing_device}
      on_select={(device) => network.select(device)} />
    {network.error && <p role="alert">{network.error}</p>}
    {context && context.connection.state !== "connected" && context.connection.error_message && <p role="alert">
      {context.connection.error_message}
      <Button size="small" variant="text" onClick={() => context.runtime.retryConnection()}>重试</Button>
    </p>}
  </section>;
});
