import { Button } from "../../../components/Button";
import { isTauri } from "@tauri-apps/api/core";
import { openExternalHttpUrl } from "../../../native-bridge/openExternalUrl";
import { CollapsibleSection } from "../../../components/CollapsibleSection";
import { observer } from "mobx-react-lite";
import { useCallback, useEffect, useRef, useState, type MouseEvent } from "react";
import type { ModelConfigurationDetail, ModelParameters, ModelSelection, ProviderSummary, ReasoningEffortKey } from "@ez-assistant/protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { SessionActionDialog } from "../../sessions/SessionActionDialog";
import { ModelFieldSelector } from "./ModelFieldSelector";
import { SettingsMessages } from "./SettingsMessages";
import { SettingsPageContainer } from "./SettingsPageContainer";
import { compileTokenDraft, sameParameters, parameterRows, provider_documents, support_options, tokenDraft, token_fields, type TokenDraft } from "./modelSettingsValues";
import styles from "./index.module.scss";

const efforts: readonly ReasoningEffortKey[] = ["low", "medium", "high", "x_high", "max"];
export const ModelFixedConfigPage = observer(function ModelFixedConfigPage(props: Readonly<{
  provider: ProviderSummary; selection: ModelSelection; manual?: boolean; onDeleted: () => void; onBack: () => void; onDirtyChange: (dirty: boolean) => void;
}>) {
  const settings = useRootStore().settings;
  const [detail, setDetail] = useState<ModelConfigurationDetail | null>(null);
  const [parameters, setParameters] = useState<ModelParameters | null>(null);
  const [tokens, setTokens] = useState<TokenDraft | null>(null);
  const [dirty, setDirty] = useState(Boolean(props.manual));
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [reference, setReference] = useState<ModelParameters | null>(null);
  const [reference_loading, setReferenceLoading] = useState(false);
  const [reference_error, setReferenceError] = useState<string | null>(null);
  const [resetting, setResetting] = useState(false);
  const origin = useRef<"manual" | "online">(props.manual ? "manual" : "online");
  const request = useRef<AbortController | null>(null);
  const reference_request = useRef<AbortController | null>(null);
  const mounted = useRef(true);
  const busy = settings.pending_action !== null;
  useEffect(() => props.onDirtyChange(dirty), [dirty, props.onDirtyChange]);
  const load = useCallback(async () => {
    request.current?.abort();
    const current = new AbortController(); request.current = current;
    setDetail(null); setParameters(null); setTokens(null); setError(null); setLoading(true);
    try {
      const result = await settings.getModelConfiguration(props.selection, current.signal, origin.current);
      if (current.signal.aborted) return;
      origin.current = result.origin;
      setDetail(result); setParameters(result.parameters); setTokens(tokenDraft(result.parameters)); setDirty(origin.current === "manual" && result.source !== "fixed");
    } catch (failure: unknown) {
      if (!current.signal.aborted) setError(failure instanceof Error ? failure.message : "读取模型参数失败。");
    } finally { if (!current.signal.aborted) setLoading(false); }
  }, [settings, props.selection, props.manual]);
  useEffect(() => {
    mounted.current = true; void load();
    return () => { mounted.current = false; request.current?.abort(); reference_request.current?.abort(); };
  }, [load]);
  function change(patch: Partial<ModelParameters>) { setParameters((current) => current ? { ...current, ...patch } : current); setDirty(true); }
  async function save() {
    if (!parameters || !tokens) return;
    let desired: ModelParameters;
    try { desired = compileTokenDraft(parameters, tokens); } catch (failure: unknown) { setError(failure instanceof Error ? failure.message : "参数无效。"); return; }
    setError(null);
    const applied = await settings.saveModelFixedConfig(props.selection, desired, detail?.origin ?? "online");
    if (!mounted.current) return;
    if (!applied) {
      // 保存超时可能发生在提交之后；只读确认匹配值，不重发写命令或覆盖草稿。
      try {
        const current = await settings.getModelConfiguration(props.selection);
        if (!mounted.current) return;
        if (current.origin !== detail?.origin || current.source !== "fixed" || !sameParameters(current.parameters, desired)) return;
        setDetail(current); settings.showNotice("已重新读取并确认固定配置。");
      } catch { return; }
    } else setDetail({ origin: applied.origin, selection: props.selection, parameters: applied.parameters, source: "fixed", updated_at_ms: applied.updated_at_ms, field_sources: {}, template_document: null, template_checked_on: null });
    setParameters(desired); setDirty(false); props.onDirtyChange(false);
  }
  async function refreshReference() {
    reference_request.current?.abort();
    const current = new AbortController(); reference_request.current = current;
    setReference(null); setReferenceError(null); setReferenceLoading(true);
    try {
      const models = await settings.listProviderModels(props.selection.provider_instance_id, current.signal);
      if (current.signal.aborted) return;
      const selected = models.find((model) => model.model_id === props.selection.model_id);
      if (!selected) throw new Error("本次在线列表中没有此模型。");
      setReference(selected.metadata);
    } catch (failure: unknown) {
      if (!current.signal.aborted) setReferenceError(failure instanceof Error ? failure.message : "刷新在线参考失败。");
    } finally { if (!current.signal.aborted) setReferenceLoading(false); }
  }
  async function reset() {
    if (!await settings.resetModelFixedConfig(props.selection, detail?.origin) || !mounted.current) return;
    setResetting(false); setDirty(false); props.onDirtyChange(false); setReference(null);
    if (detail?.origin === "manual") { props.onDeleted(); return; }
    await load(); // 重置已成功；后续在线获取失败只显示读取错误，不恢复旧固定记录。
  }
  const document_url = detail?.template_document ?? provider_documents[props.provider.connection.provider_type];
  function openDocument(event: MouseEvent<HTMLAnchorElement>) {
    if (!isTauri() || (event.button !== 0 && event.button !== 1)) return;
    event.preventDefault();
    void openExternalHttpUrl(event.currentTarget.href).catch(() => {
      if (mounted.current) setError("无法使用系统浏览器打开厂商文档，请重试。");
    });
  }
  const current_rows = parameters ? parameterRows(parameters, tokens ?? undefined) : [];
  return <SettingsPageContainer title={props.selection.model_id} on_back={props.onBack}>
    <SettingsMessages />
    {props.manual && detail?.source === "fixed" && <p>该模型已有配置，正在编辑原记录。</p>}
    {loading && <p role="status">正在读取模型参数…</p>}
    {error && <p className={styles.error_message} role="alert">{error}</p>}
    {!loading && !parameters && <Button type="button" onClick={() => void load()}>重试读取</Button>}
    {parameters && tokens && <form onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <fieldset disabled={busy} className={styles.model_fieldset}>
      <h4>运行限制</h4><div className={styles.model_form}>{token_fields.map((field) => <label key={field.key}>{field.label}
        <input aria-label={field.label} inputMode="numeric" required={field.required} value={tokens[field.key]} placeholder="待配置，请核对文档" onChange={(event) => { setTokens({ ...tokens, [field.key]: event.target.value }); setDirty(true); }} />
      </label>)}</div>
      <h4>模型能力</h4><div className={styles.model_form}>
        {([['streaming', '流式输出'], ['image_input', '图片输入'], ['tool_calls', '工具调用'], ['reasoning', '思考能力']] as const).map(([key, label]) => <ModelFieldSelector disabled={busy} key={key} label={label} value={parameters[key]} options={support_options} onChange={(value) => change({ [key]: value })} />)}
        <ModelFieldSelector disabled={busy} label="思考模式" value={parameters.reasoning_mode} options={[{ value: "unknown", label: "未知" }, { value: "unsupported", label: "不支持" }, { value: "optional", label: "可开关" }, { value: "always", label: "始终开启" }]} onChange={(reasoning_mode) => change({ reasoning_mode })} />
      </div>
      <CollapsibleSection title="思考设置" defaultOpen>
        <p>档位 → 线上值（字符串）；空置表示不支持。</p>
        <div className={styles.model_form}>{efforts.map((effort) => <label key={effort}>{effort}
          <input aria-label={`${effort} 线上值`} maxLength={128} placeholder="不支持" value={parameters.reasoning_efforts?.[effort] ?? ""} onChange={(event) => {
            const values = { ...parameters.reasoning_efforts };
            if (event.target.value) values[effort] = event.target.value;
            else delete values[effort];
            change({ reasoning_efforts: values, default_reasoning_effort: parameters.default_reasoning_effort && !values[parameters.default_reasoning_effort] ? null : parameters.default_reasoning_effort });
          }} />
        </label>)}
        <ModelFieldSelector disabled={busy} label="默认思考强度" value={parameters.default_reasoning_effort ?? ""} options={[{ value: "", label: "未设置" }, ...efforts.filter((effort) => parameters.reasoning_efforts?.[effort]).map((effort) => ({ value: effort, label: effort }))]} onChange={(value) => change({ default_reasoning_effort: value === "" ? null : value as ReasoningEffortKey })} />
        </div>
      </CollapsibleSection>
      <CollapsibleSection title="高级能力"><div className={styles.model_form}>
        {(["auto", "none", "required", "named"] as const).map((key) => <ModelFieldSelector disabled={busy} key={key} label={`工具选择 ${key}`} value={parameters.tool_choice[key]} options={support_options} onChange={(value) => change({ tool_choice: { ...parameters.tool_choice, [key]: value } })} />)}
        <ModelFieldSelector disabled={busy} label="工具图片传递" value={parameters.tool_image_projection} options={[{ value: "unknown", label: "未知" }, { value: "unsupported", label: "不支持" }, { value: "native_tool_result", label: "工具结果原生图片" }, { value: "follow_up_user_message", label: "后续用户消息图片" }]} onChange={(tool_image_projection) => change({ tool_image_projection })} />
      </div></CollapsibleSection>
      <p className={styles.model_documentation}>
        {document_url ? <a href={document_url} target="_blank" rel="noreferrer" onClick={openDocument} onAuxClick={openDocument}>查看厂商文档</a> : <span>暂无可靠的厂商参考链接，请以服务商或本地部署设置为准。</span>}
        {detail?.template_checked_on && <span>模板核查：{detail.template_checked_on}；保存前请核对型号、接入范围与套餐。</span>}
      </p>
      <div className={styles.runtime_actions}><Button variant={detail?.origin === "manual" ? "danger" : "outlined"} type="button" onClick={() => setResetting(true)} disabled={busy || detail?.source !== "fixed"}>{detail?.origin === "manual" ? "删除模型" : "重置配置"}</Button><Button type="button" disabled={busy || dirty || (props.manual && detail?.source !== "fixed")} onClick={() => void settings.validateModel(props.selection)}>{settings.pending_action === "model:validate" ? "正在测试…" : "测试已保存配置"}</Button><Button type="submit" disabled={busy}>{settings.pending_action === "model:save" ? "正在保存…" : "保存固定配置"}</Button></div>
      {detail?.source === "fixed" && <><Button type="button" onClick={() => void refreshReference()}>刷新在线参考</Button>
        {reference_loading && <p role="status">正在获取在线参考…</p>}{reference_error && <p role="alert">{reference_error}</p>}
        {reference && <><table className={styles.model_reference}><thead><tr><th>参数</th><th>当前草稿</th><th>本次在线值</th></tr></thead><tbody>{parameterRows(reference).map((row, index) => <tr key={row.label}><th>{row.label}</th><td>{current_rows[index]?.value}</td><td>{row.value}</td></tr>)}</tbody></table><Button type="button" onClick={() => { setParameters(reference); setTokens(tokenDraft(reference)); setDirty(true); }}>使用本次在线值</Button></>}
      </>}
    </fieldset></form>}
    {resetting && <SessionActionDialog title={detail?.origin === "manual" ? "删除手动模型？" : "重置固定配置？"} confirm_label={detail?.origin === "manual" ? "删除模型" : "重置配置"} is_danger is_pending={busy} on_cancel={() => setResetting(false)} on_confirm={() => void reset()}><p>{detail?.origin === "manual" ? "将删除手动模型及其参数，并放弃未保存的修改。模型选择和历史保留；若在线列表仍有同 ID，后续作为在线模型使用，否则需重新选择。" : "将删除此模型的固定参数并放弃未保存的修改，恢复在线来源。接口缺少参数时需要重新填写；模型选择和历史保留。"}</p></SessionActionDialog>}
  </SettingsPageContainer>;
});
