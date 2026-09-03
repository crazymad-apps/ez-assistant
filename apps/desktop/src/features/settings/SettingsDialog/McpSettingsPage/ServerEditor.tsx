import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import type { McpServerDraft, McpServerSnapshot, McpTransportKind } from "../../../../generated/assistant-protocol";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { SessionActionDialog } from "../../../sessions/SessionActionDialog";
import { SettingsPageContainer } from "../SettingsPageContainer";
import { SettingsMessages } from "../RuntimeSettingsPage";
import { ArgsFields, ConnectionField, SecretFields, type SecretRow } from "./ConnectionFields";
import { emptyTransport, serverDraft, validateMcpDraft } from "./draft";
import shared from "../index.module.scss";
import styles from "./index.module.scss";

export const ServerEditor = observer(function ServerEditor(props: Readonly<{
  server: McpServerSnapshot | null; revision: string; onDone: () => void;
  onDirtyChange: (dirty: boolean) => void; onDelete: (server: McpServerSnapshot) => void;
}>) {
  const settings = useRootStore().settings.mcp;
  const [draft, setDraft] = useState(() => serverDraft(props.server));
  const [rows, setRows] = useState<SecretRow[]>(() => {
    const keys = props.server?.transport === "stdio" ? props.server.environment_keys : props.server?.header_keys ?? [];
    return keys.map((name) => ({ id: name, name, change: { mode: "keep" }, existing: true }));
  });
  const [dirty, setDirty] = useState(false);
  const [discard, setDiscard] = useState(false);
  const [switch_to, setSwitchTo] = useState<McpTransportKind | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const error_ref = useRef<HTMLParagraphElement>(null);
  const busy = settings.pending_action === "save";
  useEffect(() => { props.onDirtyChange(dirty); }, [dirty, props.onDirtyChange]);
  useEffect(() => () => { props.onDirtyChange(false); settings.cancelTest(); }, [props.onDirtyChange, settings]);

  function update(patch: Partial<McpServerDraft>) { settings.cancelTest(); setDraft((value) => ({ ...value, ...patch })); setDirty(true); setError(null); }
  function leave() { if (dirty) setDiscard(true); else props.onDone(); }
  function changeTransport(type: McpTransportKind) { update({ transport: emptyTransport(type) }); setRows([]); setSwitchTo(null); }
  function candidate(): McpServerDraft | null {
    const names = rows.map((row) => row.name.trim());
    let message = validateMcpDraft(draft);
    if (names.some((name) => !name) || new Set(names).size !== names.length) message = "环境变量或请求头的名称不能为空或重复。";
    if (message) { setError(message); requestAnimationFrame(() => error_ref.current?.focus()); return null; }
    const secrets = Object.fromEntries(rows.map((row) => [row.name.trim(), row.change]));
    const transport = draft.transport.type === "stdio"
      ? { ...draft.transport, payload: { ...draft.transport.payload, environment: secrets } }
      : { ...draft.transport, payload: { ...draft.transport.payload, headers: secrets } };
    return { ...draft, display_name: draft.display_name.trim() || draft.server_key, transport };
  }
  async function save() {
    const server = candidate();
    if (server && await settings.mutate(props.revision, { type: "upsert", payload: { server } })) props.onDone();
  }
  async function reload() { await settings.load(); if (!settings.stale) { settings.clearMessages(); props.onDone(); } }
  async function copy() {
    const server = candidate();
    if (!server) return;
    try { await navigator.clipboard.writeText(JSON.stringify(server, null, 2)); setNotice("已复制当前草稿（含本次输入的新值），请妥善保管。"); }
    catch { setError("无法复制，请检查剪贴板权限。"); }
  }
  const transport = draft.transport;
  const result = settings.test_result;
  const title = props.server?.display_name ?? "添加 MCP 服务";
  return <SettingsPageContainer
    title={title}
    on_back={leave}
    back_label="返回 MCP 列表"
    actions={props.server && <button className={shared.danger_button} disabled={busy || settings.testing} onClick={() => props.server && props.onDelete(props.server)} type="button">删除服务…</button>}
  >
    <div className={styles.editor}>
      <fieldset disabled={busy || settings.testing}>
        <legend>基本信息</legend>
        <label>服务标识<input autoFocus={props.server === null} disabled={props.server !== null} onChange={(event) => update({ server_key: event.target.value })} value={draft.server_key} /></label>
        <p>用于工具身份和权限规则，创建后不可修改。</p>
        <label>显示名称<input autoFocus={props.server !== null} onChange={(event) => update({ display_name: event.target.value })} placeholder={draft.server_key} value={draft.display_name} /></label>
        <label>业务范围说明<textarea onChange={(event) => update({ description: event.target.value })} placeholder="例如：查询 GitHub 仓库、管理 Issue" rows={2} value={draft.description} /></label>
        <label className={styles.checkbox}><input checked={draft.enabled} onChange={(event) => update({ enabled: event.target.checked })} type="checkbox" />启用此服务</label>
      </fieldset>
      <fieldset disabled={busy || settings.testing}>
        <legend>连接方式</legend>
        <div className={styles.inline_row}>{(["stdio", "streamable_http"] as const).map((type) => <label className={styles.checkbox} key={type}>
          <input checked={transport.type === type} name="mcp-transport" onChange={() => setSwitchTo(type)} type="radio" />{type === "stdio" ? "本地 stdio" : "Streamable HTTP"}
        </label>)}</div>
        {props.server && <p>当前目标：{props.server.target_summary}（敏感部分不回显）</p>}
        {transport.type === "stdio" && <>
          <ConnectionField label="启动命令" change={transport.payload.command} onChange={(command) => update({ transport: { ...transport, payload: { ...transport.payload, command } } })} />
          <ArgsFields change={transport.payload.args} onChange={(args) => update({ transport: { ...transport, payload: { ...transport.payload, args } } })} />
          <ConnectionField label="工作目录 cwd" removable change={transport.payload.cwd} onChange={(cwd) => update({ transport: { ...transport, payload: { ...transport.payload, cwd } } })} />
        </>}
        {transport.type === "streamable_http" && <ConnectionField label="服务 URL" change={transport.payload.url} onChange={(url) => update({ transport: { ...transport, payload: { ...transport.payload, url } } })} />}
        <SecretFields label={transport.type === "stdio" ? "环境变量" : "请求头"} rows={rows} onChange={(values) => { settings.cancelTest(); setRows(values); setDirty(true); setError(null); }} />
        <details><summary>高级设置</summary><div className={styles.timeout_fields}>
          <label>连接超时（毫秒）<input min={1} max={60000} onChange={(event) => update({ startup_timeout_ms: event.target.value ? Number(event.target.value) : undefined })} placeholder="全局默认" type="number" value={draft.startup_timeout_ms ?? ""} /></label>
          <label>工具超时（毫秒）<input min={1} max={Number.MAX_SAFE_INTEGER} onChange={(event) => update({ tool_timeout_ms: event.target.value ? Number(event.target.value) : undefined })} placeholder="全局默认" type="number" value={draft.tool_timeout_ms ?? ""} /></label>
        </div><p>留空使用全局默认。工具超时可单独延长或缩短；连接超时仍受全局上限约束。</p></details>
      </fieldset>
      {error && <p className={styles.error} ref={error_ref} role="alert" tabIndex={-1}>{error}</p>}
      <SettingsMessages messages={settings} />
      {notice && <p className={shared.notice_message} role="status">{notice}</p>}
      {settings.configuration_conflict && <div className={styles.warning}><p>配置已被其他窗口修改，未覆盖新内容。重新载入会放弃当前草稿。</p><button onClick={() => void copy()} type="button">复制当前草稿</button><button onClick={() => void reload()} type="button">重新载入</button></div>}
      <div aria-live="polite" className={styles.test_result}>
        {settings.testing && <p>正在连接并读取工具目录…</p>}
        {result && <p data-outcome={result.outcome}>{testResultLabel(result.outcome, result.stage)} · {result.elapsed_ms} ms · {result.tool_count} 个工具{result.diagnostic && result.diagnostic.code !== "tool_description_long" && ` · ${result.diagnostic.message}`}</p>}
        {result?.diagnostic?.code === "tool_description_long" && <p className={styles.warning} role="status">警告：{result.diagnostic.message}</p>}
      </div>
      <p>测试不会保存或应用配置。本地命令以当前用户权限启动，请确认来源可信。</p>
      <div className={styles.actions}>
        <button disabled={busy || settings.testing} onClick={() => { const server = candidate(); if (server) void settings.test(server); }} type="button">测试连接</button>
        {settings.testing && <button onClick={() => settings.cancelTest()} type="button">取消测试</button>}
        <span /><button disabled={busy} onClick={leave} type="button">取消</button><button className={shared.primary_button} disabled={busy || settings.testing || settings.stale} onClick={() => void save()} type="button">保存</button>
      </div>
    </div>
    {discard && <SessionActionDialog title="放弃未保存的修改？" confirm_label="放弃修改" is_danger is_pending={false} on_cancel={() => setDiscard(false)} on_confirm={props.onDone}><p>未保存的字段和本次输入的新凭据将被丢弃。</p></SessionActionDialog>}
    {switch_to && <SessionActionDialog title="切换连接方式？" confirm_label="切换连接方式" is_danger is_pending={false} on_cancel={() => setSwitchTo(null)} on_confirm={() => changeTransport(switch_to)}><p>将清空当前连接字段及环境变量或请求头草稿；保存后才修改配置。</p></SessionActionDialog>}
  </SettingsPageContainer>;
});

function testResultLabel(outcome: string, stage: string): string {
  if (outcome === "success") return "连接测试成功";
  if (outcome === "cancelled") return "测试已取消";
  const labels: Record<string, string> = { connect: "连接", protocol: "协议", catalog: "工具目录", close: "关闭", complete: "完成" };
  return `测试失败：${labels[stage] ?? stage}阶段`;
}
