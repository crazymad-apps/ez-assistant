import { ModelStatusTags } from "./ModelStatusTags";
import { Button } from "../../../components/Button";
import { observer } from "mobx-react-lite";
import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import type { DiscoveredModel, ModelFixedConfig, ModelSelection, ProviderSummary, ProviderUsage } from "../../../generated/assistant-protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { SessionActionDialog } from "../../sessions/SessionActionDialog";
import { SettingsMessages } from "./SettingsMessages";
import { SettingsPageContainer } from "./SettingsPageContainer";
import { provider_labels } from "./modelSettingsValues";
import styles from "./index.module.scss";

export const ProviderSettingsPage = observer(function ProviderSettingsPage(props: Readonly<{
  provider: ProviderSummary; onBack: () => void; onEdit: () => void; onDelete: () => void; onAddModel: () => void; onModel: (selection: ModelSelection) => void;
}>) {
  const settings = useRootStore().settings;
  const [models, setModels] = useState<DiscoveredModel[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [fixed, setFixed] = useState<ModelFixedConfig[]>([]);
  const [fixed_loading, setFixedLoading] = useState(false);
  const [fixed_error, setFixedError] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [usage, setUsage] = useState<ProviderUsage | null>(null);
  const [usage_error, setUsageError] = useState<string | null>(null);
  const [usage_loading, setUsageLoading] = useState(false);
  const usage_request = useRef<AbortController | null>(null);
  useEffect(() => () => { usage_request.current?.abort(); }, []);

  const online_request = useRef<AbortController | null>(null);
  const fixed_request = useRef<AbortController | null>(null);
  const busy = settings.pending_action !== null;
  const provider_id = props.provider.provider_instance_id;
  const refresh = useCallback(async () => {
    online_request.current?.abort();
    const request = new AbortController(); online_request.current = request;
    setModels([]); setError(null); setLoading(true);
    try {
      const result = await settings.listProviderModels(provider_id, request.signal);
      if (!request.signal.aborted) setModels(result);
    } catch (failure: unknown) {
      if (!request.signal.aborted) setError(failure instanceof Error ? failure.message : "获取模型失败。");
    } finally { if (!request.signal.aborted) setLoading(false); }
  }, [provider_id, settings]);
  const loadFixed = useCallback(async () => {
    fixed_request.current?.abort();
    const request = new AbortController(); fixed_request.current = request;
    setFixed([]); setFixedError(null); setFixedLoading(true);
    try {
      const records = await settings.listAllFixedModels(provider_id, request.signal);
      if (!request.signal.aborted) setFixed(records);
    } catch (failure: unknown) {
      if (!request.signal.aborted) setFixedError(failure instanceof Error ? failure.message : "读取固定配置失败。");
    } finally { if (!request.signal.aborted) setFixedLoading(false); }
  }, [provider_id, settings]);
  useEffect(() => {
    void refresh(); void loadFixed();
    return () => { online_request.current?.abort(); fixed_request.current?.abort(); };
  }, [refresh, loadFixed]);
  async function loadUsage() {
    usage_request.current?.abort();
    const request = new AbortController(); usage_request.current = request;
    setUsage(null); setUsageError(null); setUsageLoading(true);
    try {
      const result = await settings.getProviderUsage(provider_id, request.signal);
      if (!request.signal.aborted) setUsage(result);
    } catch (failure: unknown) {
      if (!request.signal.aborted) setUsageError(failure instanceof Error ? failure.message : "读取引用影响失败。");
    } finally { if (!request.signal.aborted) setUsageLoading(false); }
  }
  function closeDeletion() { usage_request.current?.abort(); setDeleting(false); setUsage(null); }
  const fixed_origins = new Map(fixed.map((record) => [record.selection.model_id, record.origin]));
  const online_ids = new Set(models.map((model) => model.model_id));
  const matches = (id: string, name = "") => `${id} ${name}`.toLocaleLowerCase().includes(query.toLocaleLowerCase());
  const visible = models.filter((model) => matches(model.model_id, model.display_name ?? ""));
  const offline_fixed = fixed.filter((record) => !online_ids.has(record.selection.model_id) && matches(record.selection.model_id));
  return <SettingsPageContainer title={props.provider.connection.display_name} on_back={props.onBack} actions={<>
    <Button disabled={busy} type="button" onClick={props.onEdit}>编辑</Button><Button variant="danger" disabled={busy} type="button" onClick={() => { setDeleting(true); void loadUsage(); }}>删除服务商</Button>
  </>}>
    <SettingsMessages />
    <dl className={styles.model_details}><dt>类型</dt><dd>{provider_labels[props.provider.connection.provider_type]}</dd><dt>服务地址</dt><dd>{props.provider.connection.endpoint}</dd><dt>凭据</dt><dd>{props.provider.has_api_key ? "已设置" : "未设置"}</dd></dl>
    <div className={styles.model_section_heading}><h4>模型</h4><div className={styles.runtime_actions}><Button onClick={props.onAddModel}>添加模型</Button><Button onClick={() => { void refresh(); void loadFixed(); }}>刷新</Button></div></div>
    <input className={styles.model_search} aria-label="搜索在线模型" placeholder="搜索名称或模型 ID" value={query} onChange={(event) => setQuery(event.target.value)} />
    {loading && <p role="status">正在获取在线模型…</p>}
    {error && <p role="alert">{error}<Button onClick={() => void refresh()}>重试</Button></p>}
    {!loading && !error && visible.length === 0 && <p>本次列表没有匹配的模型。</p>}
    {fixed_loading && <p role="status">正在读取固定状态…</p>}
    {fixed_error && <p role="alert">{fixed_error}<Button onClick={() => void loadFixed()}>重试读取固定状态</Button></p>}
    <div className={styles.model_rows}>
      {visible.map((model) => <ModelRow key={model.model_id} model_id={model.model_id} display_name={model.display_name}
        label={`配置模型 ${model.model_id}`} onSelect={() => props.onModel({ provider_instance_id: provider_id, model_id: model.model_id })}>
        <ModelStatusTags origin={fixed_origins.get(model.model_id) ?? "online"} customized={fixed_origins.has(model.model_id)} configuration={model.configuration} />
      </ModelRow>)}
      {offline_fixed.map((record) => <ModelRow key={record.selection.model_id} model_id={record.selection.model_id}
        label={`编辑固定配置 ${record.selection.model_id}`} onSelect={() => props.onModel(record.selection)}>
        <ModelStatusTags origin={record.origin} customized offline={!loading} />
      </ModelRow>)}
    </div>
    {deleting && <SessionActionDialog title={`删除“${props.provider.connection.display_name}”？`} confirm_label="删除服务商" is_danger is_pending={settings.pending_action !== null} confirm_disabled={!usage || usage_loading} on_cancel={closeDeletion} on_confirm={props.onDelete}>
      {usage_loading && <p role="status">正在读取引用影响…</p>}
      {usage_error && <p role="alert">{usage_error}<Button onClick={() => void loadUsage()}>重试</Button></p>}
      {usage && <><p>本次查询：{usage.session_count} 个显式引用会话（含归档）、{usage.fixed_config_count} 条固定配置。</p>
        {usage.default_model && <p>当前默认模型使用此服务商。</p>}{usage.vision_model && <p>辅助识图模型使用此服务商。</p>}
        {usage.sessions.length > 0 && <ul>{usage.sessions.map((session) => <li key={session.session_id}>{session.title}</li>)}</ul>}
        {usage.session_count > usage.sessions.length && <p>另有 {usage.session_count - usage.sessions.length} 个会话，删除会影响全部引用。</p>}
        <p>影响数量可能随其他客户端操作变化，完成后以实际删除结果为准。</p></>}
      <p>将删除该服务商及其固定配置。已保存的模型选择和历史会保留，受影响的默认、辅助和会话模型需要重新选择。已经开始的执行使用原配置完成。</p>
    </SessionActionDialog>}
  </SettingsPageContainer>;
});

// 在线与手动模型共用行结构，来源差异仅由标签表达。
function ModelRow(props: Readonly<{
  model_id: string; display_name?: string | null; label: string; onSelect: () => void; children: ReactNode;
}>) {
  return <button type="button" aria-label={props.label} onClick={props.onSelect}>
    <span><strong>{props.display_name ?? props.model_id}</strong>
      {props.display_name && props.display_name !== props.model_id && <small>{props.model_id}</small>}
    </span>
    {props.children}
  </button>;
}
