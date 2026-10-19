import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import type { AgentCapabilityReference, CustomAgentConfig, CustomAgentSnapshot } from "@ez-assistant/protocol";
import { Button } from "../../../components/Button";
import { useAppMessage } from "../../../components/AppMessage";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import { SessionActionDialog } from "../../sessions/SessionActionDialog";
import { SettingsPageContainer } from "./SettingsPageContainer";
import styles from "./AgentSettingsPage.module.scss";

const emptyAgent = (): CustomAgentConfig => ({ name: "", description: "", prompt: "", skills: [], mcp: [] });
type Editor = Readonly<{ original: CustomAgentSnapshot | null; value: CustomAgentConfig }>;

export const AgentSettingsPage = observer(function AgentSettingsPage(props: Readonly<{ onDirtyChange: (dirty: boolean) => void }>) {
  const root = useRootStore();
  const show_message = useAppMessage();
  const client = root.local_runtime_client;
  const owner = useRef(client);
  owner.current = client;
  const [agents, setAgents] = useState<readonly CustomAgentSnapshot[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [editor, setEditor] = useState<Editor | null>(null);
  const [detail, setDetail] = useState<CustomAgentSnapshot | null>(null);
  const [deleting, setDeleting] = useState<CustomAgentSnapshot | null>(null);

  async function reload(target = client) {
    if (!target) return;
    setLoading(true);
    setError(null);
    try {
      const result = await target.command({ type: "list_custom_agents", payload: {} });
      if (owner.current !== target) return;
      setAgents(result.payload.agents);
      setUnavailable(result.payload.center_unavailable);
    } catch (cause: unknown) {
      if (owner.current === target) setError(message(cause));
    } finally {
      if (owner.current === target) setLoading(false);
    }
  }

  useEffect(() => {
    setAgents([]);
    setEditor(null);
    setDetail(null);
    setUnavailable(false);
    void reload(client);
  }, [client]);

  function startEdit(agent: CustomAgentSnapshot | null) {
    setError(null);
    setDetail(null);
    setEditor({ original: agent, value: agent ? structuredClone(agent.config) : emptyAgent() });
    props.onDirtyChange(false);
  }

  function patch(patch: Partial<CustomAgentConfig>) {
    setEditor((current) => current && { ...current, value: { ...current.value, ...patch } });
    props.onDirtyChange(true);
  }

  function leaveEditor() {
    setEditor(null);
    props.onDirtyChange(false);
  }

  async function save() {
    if (!client || !editor || busy) return;
    setBusy(true); setError(null);
    try {
      const result = await client.command({ type: "save_personal_agent", payload: {
        expected_revision: editor.original?.revision ?? null, config: editor.value,
      } });
      if (owner.current !== client) return;
      show_message(`已保存 ${result.payload.config.name}。`);
      leaveEditor();
      await reload(client);
    } catch (cause: unknown) {
      if (owner.current === client) setError(message(cause));
    } finally {
      if (owner.current === client) setBusy(false);
    }
  }

  async function remove() {
    if (!client || !deleting || busy) return;
    setBusy(true); setError(null);
    try {
      await client.command({ type: "delete_personal_agent", payload: {
        name: deleting.config.name, expected_revision: deleting.revision,
      } });
      if (owner.current !== client) return;
      setDeleting(null);
      setDetail(null);
      show_message("个人智能体已删除，已有会话的冻结配置仍保留。");
      await reload(client);
    } catch (cause: unknown) {
      if (owner.current === client) setError(message(cause));
    } finally {
      if (owner.current === client) setBusy(false);
    }
  }

  const title = editor ? (editor.original ? "编辑个人智能体" : "创建个人智能体") : detail ? detail.config.name : "智能体";
  return <>
    <SettingsPageContainer
      title={title}
      on_back={editor ? leaveEditor : detail ? () => setDetail(null) : undefined}
      actions={!editor && !detail ? <><Button disabled={loading} onClick={() => void reload()}><Icon name="refresh" size={15} />刷新</Button><Button disabled={!client} onClick={() => startEdit(null)}><Icon name="plus" size={15} />创建</Button></> : undefined}
    >
      <div className={styles.page}>
        {error && <p className={styles.error} role="alert">{error}</p>}
        {unavailable && <p className={styles.warning}>企业目录暂不可读取，以下企业条目是本次连接中最后一次有效结果。</p>}
        {loading && <p role="status">正在读取智能体…</p>}
        {editor ? <div className={styles.editor}>
          <label>名称<input disabled={busy || Boolean(editor.original)} maxLength={64} onChange={(event) => patch({ name: event.target.value })} value={editor.value.name} /></label>
          <label>说明<textarea disabled={busy} maxLength={4096} onChange={(event) => patch({ description: event.target.value })} rows={2} value={editor.value.description} /></label>
          <label>完整提示词<textarea className={styles.prompt} disabled={busy} onChange={(event) => patch({ prompt: event.target.value })} rows={10} value={editor.value.prompt} /></label>
          <ReferenceEditor disabled={busy} label="默认技能" onChange={(skills) => patch({ skills })} value={editor.value.skills} />
          <ReferenceEditor disabled={busy} label="默认 MCP 服务" onChange={(mcp) => patch({ mcp })} value={editor.value.mcp} />
          <div className={styles.actions}><Button disabled={busy} onClick={leaveEditor}>取消</Button><Button disabled={busy || !editor.value.name.trim() || !editor.value.prompt.trim()} onClick={() => void save()} variant="primary">{busy ? "正在保存…" : "保存智能体"}</Button></div>
        </div> : detail ? <div className={styles.detail}>
          <p className={styles.source}>{detail.source.type === "personal" ? "个人配置" : "企业中心"}</p>
          <p>{detail.config.description || "暂无说明"}</p>
          <h4>完整提示词</h4><pre>{detail.config.prompt}</pre>
          <h4>默认技能</h4><CapabilityList items={detail.config.skills} />
          <h4>默认 MCP 服务</h4><CapabilityList items={detail.config.mcp} />
          {detail.source.type === "personal" ? <div className={styles.actions}><Button onClick={() => startEdit(detail)}>编辑</Button><Button disabled={busy} onClick={() => setDeleting(detail)} variant="danger">删除</Button></div> : <p className={styles.warning}>企业配置只读。请在企业管理后台修改。</p>}
        </div> : <div className={styles.list}>
          {agents.length === 0 && !loading && <p>还没有可用的自定义智能体。</p>}
          {agents.map((agent) => <button className={styles.item} key={`${agent.source.type}:${agent.source.type === "center" ? agent.source.center_id : ""}:${agent.config.name}`} onClick={() => setDetail(agent)} type="button">
            <span><strong>{agent.config.name}</strong><small>{agent.config.description || "暂无说明"}</small></span>
            <em>{agent.source.type === "personal" ? "个人" : "企业"}</em><Icon name="chevron-right" size={16} />
          </button>)}
        </div>}
      </div>
    </SettingsPageContainer>
    {deleting && <SessionActionDialog title="删除个人智能体？" confirm_label="删除" is_danger is_pending={busy} on_cancel={() => setDeleting(null)} on_confirm={() => void remove()}><p>删除 {deleting.config.name} 后，新会话无法再选择它；已有会话会保留创建时的配置。</p></SessionActionDialog>}
  </>;
});

function CapabilityList({ items }: Readonly<{ items: readonly AgentCapabilityReference[] }>) {
  return items.length ? <ul>{items.map((item) => <li key={item.name}>{item.name}{item.plugin ? ` · 插件 ${item.plugin}` : " · 独立能力"}</li>)}</ul> : <p>未声明</p>;
}

function ReferenceEditor(props: Readonly<{ label: string; value: readonly AgentCapabilityReference[]; disabled: boolean; onChange: (value: AgentCapabilityReference[]) => void }>) {
  function update(index: number, patch: Partial<AgentCapabilityReference>) {
    props.onChange(props.value.map((item, position) => position === index ? { ...item, ...patch } : item));
  }
  return <fieldset className={styles.references}><legend>{props.label}</legend>
    {props.value.map((item, index) => <div className={styles.reference} key={index}>
      <input aria-label={`${props.label}名称 ${index + 1}`} disabled={props.disabled} onChange={(event) => update(index, { name: event.target.value })} placeholder="能力名称" value={item.name} />
      <input aria-label={`${props.label}所属插件 ${index + 1}`} disabled={props.disabled} onChange={(event) => update(index, { plugin: event.target.value || undefined })} placeholder="插件名，留空为独立能力" value={item.plugin ?? ""} />
      <Button aria-label={`移除${props.label} ${index + 1}`} disabled={props.disabled} iconOnly onClick={() => props.onChange(props.value.filter((_, position) => position !== index))}><Icon name="x" size={14} /></Button>
    </div>)}
    <Button disabled={props.disabled || props.value.length >= 256} onClick={() => props.onChange([...props.value, { name: "" }])}><Icon name="plus" size={14} />添加{props.label}</Button>
  </fieldset>;
}

function message(error: unknown): string { return error instanceof Error ? error.message : "智能体操作失败，请重试。"; }
