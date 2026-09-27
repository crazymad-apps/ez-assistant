import { useEffect, useRef, useState } from "react";
import type { PluginOperationSnapshot, PluginPackageSelection } from "@ez-assistant/protocol";
import { Button } from "../../../components/Button";
import { Dialog } from "../../../components/Dialog";
import type { RuntimeClient } from "../../../runtime-client/RuntimeClient";
import styles from "./PluginSettingsPage.module.scss";

export type PluginOperationIntent =
  | Readonly<{ type: "install"; packages: readonly PluginPackageSelection[]; file?: File }>
  | Readonly<{ type: "uninstall"; name: string; sha256: string; clear_data: boolean }>;

const terminal = (phase: PluginOperationSnapshot["phase"]) =>
  phase === "succeeded" || phase === "failed" || phase === "cancelled" || phase === "interrupted";

export function PluginOperationDialog(props: Readonly<{
  client: RuntimeClient;
  intent: PluginOperationIntent | null;
  initial_snapshot?: PluginOperationSnapshot | null;
  on_close: () => void;
  on_complete: () => void;
  on_retry?: () => void;
  auto_begin?: boolean;
}>) {
  const [snapshot, setSnapshot] = useState<PluginOperationSnapshot | null>(props.initial_snapshot ?? null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [cancel_pending, setCancelPending] = useState(false);
  const auto_begin_started = useRef(false);
  const done_ref = useRef(false);
  const upload_abort = useRef<AbortController | null>(null);
  const key = snapshot?.key;
  const cleanup_blocked = Boolean(snapshot?.cleanup_required);
  const running = pending || Boolean(snapshot && !terminal(snapshot.phase) && !cleanup_blocked);

  useEffect(() => {
    if (props.auto_begin && !auto_begin_started.current && !snapshot) {
      auto_begin_started.current = true;
      void begin();
    }
  }, [props.auto_begin]);

  useEffect(() => {
    // 查看历史操作只展示已保存的结果，不再次发出本次操作完成通知。
    if (props.intent && snapshot?.phase === "succeeded" && !done_ref.current) {
      done_ref.current = true;
      props.on_complete();
    }
  }, [snapshot?.phase, props.intent, props.on_complete]);

  useEffect(() => {
    if (!key || snapshot && terminal(snapshot.phase) && (!snapshot.cleanup_required || snapshot.phase === "failed")) return undefined;
    let active = true;
    async function refresh() {
      try {
        const next = await props.client.command({ type: "get_plugin_operation", payload: { key: key! } });
        if (!active) return;
        setSnapshot((current) => current && terminal(current.phase) && !current.cleanup_required ? current : next.payload);
      } catch (cause: unknown) {
        if (active) setError(message(cause));
      }
    }
    void refresh();
    const timer = window.setInterval(() => void refresh(), 600);
    return () => { active = false; window.clearInterval(timer); };
  }, [key, snapshot?.phase, snapshot?.cleanup_required, props.client]);

  async function begin() {
    if (running || !props.intent) return;
    setPending(true); setError(null);
    try {
      const operation_key = typeof crypto.randomUUID === "function"
        ? `plugin-${crypto.randomUUID()}`
        : `plugin-${Date.now()}-${Math.random().toString(16).slice(2)}`;
      let result: PluginOperationSnapshot;
      if (props.intent.type === "install") {
        result = (await props.client.command({ type: "begin_plugin_install", payload: {
          key: operation_key, packages: [...props.intent.packages],
        } })).payload;
      } else {
        result = (await props.client.command({ type: "uninstall_plugin", payload: {
          key: operation_key, name: props.intent.name,
          expected_active_sha256: props.intent.sha256, clear_data: props.intent.clear_data,
        } })).payload;
      }
      setSnapshot(result);
      if (props.intent.type === "install" && props.intent.file) {
        const controller = new AbortController();
        upload_abort.current = controller;
        try {
          const uploaded = await props.client.uploadPluginPackage(operation_key, props.intent.file, controller.signal);
          setSnapshot(uploaded);
        } catch (cause: unknown) {
          // Host 仍是操作权威；上传结果未知时继续轮询原 key。
          setError(message(cause));
        } finally {
          upload_abort.current = null;
        }
      }
    } catch (cause: unknown) {
      setError(message(cause));
    } finally {
      setPending(false);
    }
  }

  async function cancel() {
    if (!key || cancel_pending) return;
    setCancelPending(true);
    upload_abort.current?.abort();
    try {
      const result = await props.client.command({ type: "cancel_plugin_operation", payload: { key } });
      setSnapshot(result.payload);
    } catch (cause: unknown) {
      setError(message(cause));
    } finally {
      setCancelPending(false);
    }
  }

  const close = () => { if (!running) props.on_close(); };
  const action = props.intent?.type === "uninstall" || snapshot?.kind === "uninstall" ? "卸载"
    : snapshot?.kind === "update" || props.intent?.type === "install" && props.intent.packages.some((item) => item.expected_active_sha256) ? "更新" : "安装";
  const names = props.intent?.type === "install" ? props.intent.packages.map((item) => item.name) : props.intent?.type === "uninstall" ? [props.intent.name] : snapshot?.committed_names ?? [];
  const percent = snapshot?.received_bytes != null && snapshot.total_bytes
    ? Math.min(100, Math.round(snapshot.received_bytes / snapshot.total_bytes * 100)) : null;
  const download_complete = snapshot?.phase === "preparing" && snapshot.received_bytes != null
    && snapshot.total_bytes != null && snapshot.received_bytes >= snapshot.total_bytes;
  return <Dialog aria_label={`${action}插件`} backdrop_class_name={styles.backdrop} dialog_class_name={styles.operation_dialog} dismissible={!running} on_close={close}>
    <h3>{action}插件</h3>
    <p>{names.join("、")}</p>
    {!snapshot && props.intent?.type === "install" && <ul className={styles.package_list}>{props.intent.packages.map((item) => <li key={item.name}><strong>{item.name}</strong><span>{item.version} · {formatBytes(item.size_bytes)} · {item.origin.type === "center" ? "企业中心" : "个人上传"}</span><code title={item.sha256}>{item.sha256}</code></li>)}</ul>}
    {!snapshot && props.intent?.type === "uninstall" && <p>{props.intent.clear_data ? "同时清除该插件的用户配置与运行数据。" : "保留该插件的用户配置与运行数据。"}项目产物不受影响。</p>}
    {snapshot && <div className={styles.progress} role="status">
      <strong>{snapshot.cleanup_required ? "插件目录清理受阻" : phaseLabel(snapshot.phase, download_complete)}</strong>
      {snapshot.received_bytes != null && <><progress max={snapshot.total_bytes || 1} value={snapshot.received_bytes} /><span>文件传输：{formatBytes(snapshot.received_bytes)} / {snapshot.total_bytes ? formatBytes(snapshot.total_bytes) : "总量未知"}{percent !== null ? ` · ${percent}%` : ""}</span></>}
      {snapshot.committed_names.length > 0 && <span>已完成：{snapshot.committed_names.join("、")}</span>}
      {snapshot.error_code && <span className={styles.error}>原因：{snapshot.error_code}</span>}
      {snapshot.cleanup_required && <span className={styles.error}>{snapshot.phase === "failed" ? "此失败记录不影响其他插件操作。" : "应用会自动重试清理，其他插件操作可继续。"}</span>}
    </div>}
    {pending && !snapshot && <p role="status">正在创建操作…</p>}
    {error && <p className={styles.error} role="alert">{error}</p>}
    <footer className={styles.actions}>
      {running ? <Button disabled={cancel_pending || !key} onClick={() => void cancel()}>{cancel_pending ? "正在取消…" : "取消操作"}</Button>
        : <Button onClick={close}>关闭</Button>}
      {!snapshot && props.intent && !props.auto_begin && <Button disabled={pending} onClick={() => void begin()} variant={props.intent.type === "uninstall" ? "danger" : "primary"}>{props.intent.type === "install" ? `确认并开始${action}` : "确认卸载"}</Button>}
      {snapshot && terminal(snapshot.phase) && snapshot.phase !== "succeeded" && !snapshot.cleanup_required && props.on_retry
        && <Button onClick={props.on_retry}>重新检查并重试</Button>}
    </footer>
  </Dialog>;
}

function phaseLabel(phase: PluginOperationSnapshot["phase"], download_complete: boolean): string {
  switch (phase) {
    case "accepted": return "等待开始";
    case "preparing": return download_complete ? "文件已接收，正在校验并解包" : "正在下载、校验或解包";
    case "committing": return "正在发布插件";
    case "cleaning": return "正在清理过程文件";
    case "succeeded": return "插件操作已完成";
    case "failed": return "插件操作失败";
    case "cancelled": return "已取消";
    case "interrupted": return "操作中断，请检查 Host 状态";
  }
}

function formatBytes(value: number): string { return value >= 1024 ** 2 ? `${(value / 1024 ** 2).toFixed(1)} MiB` : `${Math.round(value / 1024)} KiB`; }
function message(error: unknown): string { return error instanceof Error ? error.message : "插件操作失败，请重试。"; }
