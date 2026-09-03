import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import { Icon } from "../../../../components/Icon";
import type { McpServerSnapshot } from "../../../../generated/assistant-protocol";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { SessionActionDialog } from "../../../sessions/SessionActionDialog";
import { SettingsPageContainer } from "../SettingsPageContainer";
import { SettingsMessages } from "../RuntimeSettingsPage";
import { ImportDialog } from "./ImportDialog";
import { ServerEditor } from "./ServerEditor";
import shared from "../index.module.scss";
import styles from "./index.module.scss";

export const McpSettingsPage = observer(function McpSettingsPage(props: Readonly<{ onDirtyChange: (dirty: boolean) => void }>) {
  const root = useRootStore();
  const settings = root.settings.mcp;
  const application = root.projection.application;
  const available = application?.capabilities.mcp_management ?? false;
  const [search, setSearch] = useState("");
  const [editing, setEditing] = useState<{ server: McpServerSnapshot | null; revision: string } | null>(null);
  const [import_revision, setImportRevision] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<{ server: McpServerSnapshot; revision: string } | null>(null);
  const trigger = useRef<HTMLElement | null>(null);
  const snapshot = settings.configuration;
  const busy = settings.pending_action === "save";
  const readonly = settings.stale || !snapshot || busy;
  // 复用现有 Application 失效刷新：配置保存、Command 结算及 SSE gap 后都会重新读取。
  useEffect(() => { if (available) void settings.load(); }, [available, application, settings]);
  useEffect(() => () => settings.deactivate(), [settings]);
  function beginEdit(server: McpServerSnapshot | null) {
    if (!snapshot) return;
    trigger.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    settings.clearMessages(); setEditing({ server, revision: snapshot.revision });
  }
  function done() { setEditing(null); props.onDirtyChange(false); requestAnimationFrame(() => trigger.current?.focus()); }
  function beginDelete(server: McpServerSnapshot) {
    // 删除确认绑定用户已查看的版本，不能使用后台刷新后尚未展示的配置 revision。
    if (editing) setDeleting({ server, revision: editing.revision });
  }
  async function remove() {
    if (!deleting) return;
    if (await settings.mutate(deleting.revision, { type: "remove", payload: { server_key: deleting.server.server_key } })) { setDeleting(null); done(); }
  }
  const normalized = search.trim().toLowerCase();
  const servers = snapshot?.servers.filter((server) => `${server.server_key} ${server.display_name} ${server.description}`.toLowerCase().includes(normalized)) ?? [];
  let content;
  if (!available) {
    content = <SettingsPageContainer title="MCP"><p className={styles.error}>当前运行时不支持 MCP 管理，请重启升级运行时。</p></SettingsPageContainer>;
  } else if (editing) {
    content = <ServerEditor {...editing} onDelete={beginDelete} onDirtyChange={props.onDirtyChange} onDone={done} />;
  } else {
    content = <SettingsPageContainer title="MCP" actions={<><button disabled={readonly} onClick={() => { settings.clearMessages(); setImportRevision(snapshot!.revision); }} type="button">导入配置</button><button className={shared.primary_button} disabled={readonly} onClick={() => beginEdit(null)} type="button"><Icon name="plus" size={14} />添加服务</button></>}>
      <div className={styles.page}>
        <div className={styles.search}><input aria-label="搜索 MCP 服务" onChange={(event) => setSearch(event.target.value)} placeholder="搜索服务名称或业务范围" value={search} /><button aria-label="重新读取配置" disabled={settings.loading} onClick={() => { settings.clearMessages(); void settings.load(); }} title="重新读取配置" type="button"><Icon name="refresh" size={16} /></button></div>
        <SettingsMessages messages={settings} />
        {settings.stale && <p className={styles.error}>列表已过期，仅可查看。重新读取成功后可继续修改。</p>}
        {snapshot?.diagnostics.map((diagnostic, index) => <p className={diagnostic.code === "tool_description_long" ? styles.warning : styles.error} key={index} role={diagnostic.code === "tool_description_long" ? "status" : "alert"}>{diagnostic.code === "tool_description_long" && "警告："}{diagnostic.server_key && `${diagnostic.server_key}：`}{diagnostic.message}</p>)}
        {settings.loading && !snapshot && <p role="status">正在读取 MCP 配置…</p>}
        {snapshot && servers.length === 0 && <p className={styles.empty}>{normalized ? "没有匹配的服务" : "还没有 MCP 服务"}</p>}
        <div className={styles.server_list}>{servers.map((server) => <article key={server.server_key}>
          <button className={styles.server_body} disabled={readonly} onClick={() => beginEdit(server)} type="button">
            <i data-state={server.needs_refresh ? "pending" : server.runtime_state} />
            <span><strong>{server.display_name} <code>{server.server_key}</code></strong><em>{server.description || "未配置业务范围"}</em><small>{server.transport === "stdio" ? "本地 stdio" : "Streamable HTTP"} · {stateLabel(server)}</small></span><Icon name="chevron-right" size={15} />
          </button>
          <label className={shared.switch}><input aria-label={`启用 ${server.display_name}`} checked={server.enabled} disabled={readonly} onChange={(event) => { void settings.mutate(snapshot!.revision, { type: "set_enabled", payload: { server_key: server.server_key, enabled: event.target.checked } }); }} type="checkbox" /><span /><b>{server.enabled ? "已启用" : "已禁用"}</b></label>
        </article>)}</div>
      </div>
    </SettingsPageContainer>;
  }
  return <>{content}
    {import_revision !== null && <ImportDialog revision={import_revision} onClose={() => setImportRevision(null)} onDirtyChange={props.onDirtyChange} />}
    {deleting && <SessionActionDialog title="删除 MCP 服务？" confirm_label="删除服务" is_danger is_pending={busy} on_cancel={() => setDeleting(null)} on_confirm={() => void remove()}><p>从本机用户配置中删除 {deleting.server.display_name}（{deleting.server.server_key}）。当前连接在刷新后关闭；不会删除外部数据、安装包、既有权限规则或历史记录。</p></SessionActionDialog>}
  </>;
});

function stateLabel(server: McpServerSnapshot): string {
  if (server.needs_refresh) return `配置待刷新 · ${server.tool_count} 个工具`;
  if (server.runtime_state === "connected") return `已连接 · ${server.tool_count} 个工具`;
  if (server.runtime_state === "connected_without_tools") return "已连接 · 无工具";
  if (server.runtime_state === "disabled") return "已禁用";
  return "尚未连接";
}
