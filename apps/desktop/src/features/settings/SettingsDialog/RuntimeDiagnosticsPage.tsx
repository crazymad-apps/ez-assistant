import { copyText } from "../../../platform/clipboard";
import { observer } from "mobx-react-lite";
import { useRootStore } from "../../../stores/RootStoreContext";
import { SettingsPageContainer } from "./SettingsPageContainer";
import { SettingsMessages } from "./SettingsMessages";
import { connectionLabel, configurationLabel, formatDateTime } from "./runtimePresentation";
import styles from "./index.module.scss";

export const RuntimeDiagnosticsPage = observer(function RuntimeDiagnosticsPage({ on_back }: Readonly<{ on_back: () => void }>) {
  const store = useRootStore();
  const settings = store.settings;
  const connection = store.connection;
  const capabilities = connection.capabilities;
  const runtime_lifecycle = store.projection.application?.runtime_lifecycle ?? null;
  const status = settings.status ?? store.projection.application?.configuration ?? null;

  async function copyDiagnostics() {
    const diagnostics = [
      `connection=${connection.state}`,
      `runtime_lifecycle=${runtime_lifecycle ?? "-"}`,
      `instance_id=${connection.instance_id ?? "-"}`,
      `address=${connection.address ?? "-"}`,
      `runtime_version=${capabilities?.runtime_version ?? "-"}`,
      `min_compatible_version=${capabilities?.min_compatible_version ?? "-"}`,
      `last_connected_at=${formatDateTime(connection.last_connected_at_ms)}`,
      `last_error_code=${connection.last_error_code ?? "-"}`,
      `configuration=${status?.state ?? "-"}`,
      `configuration_revision=${status?.revision ?? "-"}`,
      `configuration_path=${status?.config_path ?? "-"}`,
      `features=${capabilities?.features?.join(",") ?? "-"}`,
    ].join("\n");
    await copyText(diagnostics);
    settings.showNotice("诊断信息已复制。");
  }

  return (
    <SettingsPageContainer
      actions={(
        <button disabled={settings.pending_action !== null} onClick={() => void settings.reloadConfiguration()} type="button">
          重新加载配置
        </button>
      )}
      title="状态与诊断"
      on_back={on_back}
      back_label="返回 Runtime"
    >
      <div className={styles.runtime_grid}>
        <article>
          <h4>连接</h4>
          <dl>
            <div><dt>状态</dt><dd>{connectionLabel(connection.state)}</dd></div>
            <div><dt>生命周期</dt><dd>{runtime_lifecycle ?? "—"}</dd></div>
            <div><dt>实例</dt><dd title={connection.instance_id ?? undefined}>{connection.instance_id ?? "—"}</dd></div>
            <div><dt>Host 地址</dt><dd>{connection.address ?? "—"}</dd></div>
            <div><dt>运行时版本</dt><dd>{capabilities?.runtime_version ?? "—"}</dd></div>
            <div><dt>最低兼容软件版本</dt><dd>{capabilities?.min_compatible_version ?? "—"}</dd></div>
            <div><dt>最近连接</dt><dd>{formatDateTime(connection.last_connected_at_ms)}</dd></div>
            <div><dt>错误分类</dt><dd>{connection.last_error_code ?? "—"}</dd></div>
          </dl>
          <div className={styles.runtime_actions}>
            <button onClick={() => store.retryConnection()} type="button">重新连接</button>
          </div>
        </article>
        <article>
          <h4>配置</h4>
          <dl>
            <div><dt>状态</dt><dd data-state={status?.state}>{configurationLabel(status?.state)}</dd></div>
            <div><dt>结构版本</dt><dd>{status?.schema_version ?? "—"}</dd></div>
            <div><dt>修订</dt><dd title={status?.revision ?? undefined}>{status?.revision?.slice(0, 12) ?? "—"}</dd></div>
          </dl>
        </article>
      </div>
      <article className={styles.diagnostic_card}>
        <div className={styles.diagnostic_heading}>
          <h4>诊断信息</h4>
          <p title={status?.config_path ?? undefined}>
            {status?.config_path ?? "尚未创建运行时配置文件"}
          </p>
        </div>
        <button onClick={() => void copyDiagnostics()} type="button">复制诊断</button>
      </article>
      {status?.issues.length ? (
        <div className={styles.issue_list}>
          {status.issues.map((issue, index) => <p key={`${issue.code}-${index}`}>{issue.message}</p>)}
        </div>
      ) : null}
      <SettingsMessages />
    </SettingsPageContainer>
  );
});
