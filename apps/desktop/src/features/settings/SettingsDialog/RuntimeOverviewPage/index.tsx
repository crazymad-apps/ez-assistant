import { observer } from "mobx-react-lite";
import { Icon } from "../../../../components/Icon";
import { Button } from "../../../../components/Button";
import type { RuntimeSettingsPageId } from "../../../../stores/SettingsStore";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { SettingsPageContainer } from "../SettingsPageContainer";
import { SettingsMessages } from "../SettingsMessages";
import { connectionLabel } from "../runtimePresentation";
import styles from "./index.module.scss";

export const RuntimeOverviewPage = observer(function RuntimeOverviewPage(props: Readonly<{
  onNavigate: (page: RuntimeSettingsPageId) => void;
  target: string;
  local: boolean;
  desktop: boolean;
}>) {
  const root = useRootStore();
  const connection = root.connection;

  return <SettingsPageContainer title="Runtime">
    <section className={styles.connection} aria-label="当前连接">
      <div className={styles.eyebrow}>当前连接</div>
      <div className={styles.target_row}>
        <span className={styles.target_icon}><Icon name={props.local ? "terminal" : "globe"} size={21} /></span>
        <div className={styles.target}>
          <strong title={props.target}>{props.target}</strong>
          <span className={styles.status} data-connected={connection.state === "connected"}>
            <i />{connectionLabel(connection.state)}
            {connection.capabilities?.runtime_version && <span>· v{connection.capabilities.runtime_version}</span>}
          </span>
        </div>
        {props.desktop && <Button variant="text" onClick={() => props.onNavigate("runtime_connection")}>切换<Icon name="chevron-right" size={14} /></Button>}
      </div>
    </section>
    <div className={styles.sections}>
      <button className={styles.row} onClick={() => props.onNavigate("host_access")} type="button">
        <span className={styles.icon}><Icon name="shield" size={18} /></span>
        <strong className={styles.row_text}>访问设置</strong>
        <Icon name="chevron-right" size={16} />
      </button>
      <button className={styles.row} onClick={() => props.onNavigate("runtime_diagnostics")} type="button">
        <span className={styles.icon}><Icon name="file" size={18} /></span>
        <strong className={styles.row_text}>状态与诊断</strong>
        <Icon name="chevron-right" size={16} />
      </button>
      {props.desktop && <button className={styles.row} onClick={() => props.onNavigate("runtime_local")} type="button">
        <span className={styles.icon}><Icon name="terminal" size={18} /></span>
        <strong className={styles.row_text}>本机与客户端</strong>
        <Icon name="chevron-right" size={16} />
      </button>}
    </div>
    <SettingsMessages />
  </SettingsPageContainer>;
});
