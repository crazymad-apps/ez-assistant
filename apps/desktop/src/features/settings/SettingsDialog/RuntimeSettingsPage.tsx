import { observer } from "mobx-react-lite";
import { useState } from "react";
import { SelectionPopover, type SelectionOption } from "../../../components/SelectionPopover";
import type { DesktopCloseBehavior } from "../../../native-bridge/desktopPreferences";
import { openRuntimeHome } from "../../../native-bridge/runtimeHome";
import { useRootStore } from "../../../stores/RootStoreContext";
import { SettingsPageContainer } from "./SettingsPageContainer";
import styles from "./index.module.scss";

const close_behavior_options: readonly SelectionOption<DesktopCloseBehavior>[] = [
  { value: "hide_to_tray", label: "隐藏到托盘", description: "关闭主窗口后保留桌面客户端" },
  { value: "quit_desktop", label: "退出客户端", description: "关闭主窗口时进入退出确认" },
];

export const RuntimeSettingsPage = observer(function RuntimeSettingsPage() {
  const store = useRootStore();
  const settings = store.settings;
  const connection = store.connection;
  const capabilities = connection.capabilities;
  const runtime_lifecycle = store.projection.application?.runtime_lifecycle ?? null;
  const status = settings.status ?? store.projection.application?.configuration ?? null;
  const [close_behavior_open, setCloseBehaviorOpen] = useState(false);

  async function copyDiagnostics() {
    const diagnostics = [
      `connection=${connection.state}`,
      `runtime_lifecycle=${runtime_lifecycle ?? "-"}`,
      `instance_id=${connection.instance_id ?? "-"}`,
      `address=${connection.address ?? "-"}`,
      `runtime_version=${capabilities?.runtime_version ?? "-"}`,
      `protocol_version=${capabilities?.protocol_version ?? "-"}`,
      `last_connected_at=${formatDateTime(connection.last_connected_at_ms)}`,
      `last_error_code=${connection.last_error_code ?? "-"}`,
      `configuration=${status?.state ?? "-"}`,
      `configuration_revision=${status?.revision ?? "-"}`,
      `configuration_path=${status?.config_path ?? "-"}`,
      `features=${capabilities?.features?.join(",") ?? "-"}`,
    ].join("\n");
    await navigator.clipboard.writeText(diagnostics);
    settings.showNotice("诊断信息已复制。");
  }

  async function openHome() {
    try {
      await openRuntimeHome();
    } catch (error: unknown) {
      settings.showError(error instanceof Error ? error.message : "无法打开运行时目录。");
    }
  }

  return (
    <SettingsPageContainer
      actions={(
        <button disabled={settings.pending_action !== null} onClick={() => void settings.reloadConfiguration()} type="button">
          重新加载配置
        </button>
      )}
      title="运行时"
    >
      <div className={styles.runtime_grid}>
        <article>
          <h4>连接</h4>
          <dl>
            <div><dt>状态</dt><dd>{connectionLabel(connection.state)}</dd></div>
            <div><dt>生命周期</dt><dd>{runtime_lifecycle ?? "—"}</dd></div>
            <div><dt>实例</dt><dd title={connection.instance_id ?? undefined}>{connection.instance_id ?? "—"}</dd></div>
            <div><dt>本地地址</dt><dd>{connection.address ?? "—"}</dd></div>
            <div><dt>运行时版本</dt><dd>{capabilities?.runtime_version ?? "—"}</dd></div>
            <div><dt>协议版本</dt><dd>{capabilities?.protocol_version ?? "—"}</dd></div>
            <div><dt>最近连接</dt><dd>{formatDateTime(connection.last_connected_at_ms)}</dd></div>
            <div><dt>错误分类</dt><dd>{connection.last_error_code ?? "—"}</dd></div>
          </dl>
          <div className={styles.runtime_actions}>
            <button onClick={() => store.retryConnection()} type="button">重新连接</button>
            <button onClick={() => void openHome()} type="button">打开运行时目录</button>
          </div>
        </article>
        <article>
          <h4>配置</h4>
          <dl>
            <div><dt>状态</dt><dd data-state={status?.state}>{configurationLabel(status?.state)}</dd></div>
            <div><dt>默认模型</dt><dd>{status?.default_model ?? "—"}</dd></div>
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
      <article className={styles.lifecycle_card}>
        <h4>桌面生命周期</h4>
        <SelectionPopover
          aria_label="关闭主窗口时"
          content_width="content"
          open={close_behavior_open}
          on_open_change={setCloseBehaviorOpen}
          on_select={(value) => store.desktop_lifecycle.setCloseBehavior(value)}
          options={close_behavior_options}
          selected={store.desktop_lifecycle.close_behavior}
          trigger_class_name={styles.lifecycle_select}
          trigger_variant="compact"
        />
        <div className={styles.lifecycle_actions}>
          <button onClick={() => store.desktop_lifecycle.request("restart_runtime")} type="button">重启运行时</button>
          <button className={styles.danger_button} onClick={() => store.desktop_lifecycle.request("stop_runtime")} type="button">停止运行时</button>
        </div>
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

export const SettingsMessages = observer(function SettingsMessages(props: Readonly<{ messages?: Readonly<{ error_message: string | null; notice_message: string | null }> }>) {
  const fallback = useRootStore().settings;
  const settings = props.messages ?? fallback;
  return (
    <>
      {settings.error_message && <p className={styles.error_message} role="alert">{settings.error_message}</p>}
      {settings.notice_message && <p className={styles.notice_message} role="status">{settings.notice_message}</p>}
    </>
  );
});

function connectionLabel(state: string): string {
  return ({
    connected: "已连接",
    reconnecting: "重连中",
    disconnected: "已断开",
    component_mismatch: "组件不兼容",
    stopping_runtime: "停止中",
    restarting_runtime: "重启中",
    runtime_stopped: "已停止",
  } as Record<string, string>)[state] ?? "连接中";
}

function configurationLabel(state?: string): string {
  return ({ ready: "可用", degraded: "部分可用", invalid: "无效", missing: "未配置" } as Record<string, string>)[state ?? ""] ?? "—";
}

function formatDateTime(value: number | null): string {
  if (value === null) return "—";
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(new Date(value));
}
