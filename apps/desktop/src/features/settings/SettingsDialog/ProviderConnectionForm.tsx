import { Button } from "../../../components/Button";
import { CollapsibleSection } from "../../../components/CollapsibleSection";
import { InlineIconButton } from "../../../components/InlineIconButton";
import { observer } from "mobx-react-lite";
import { useEffect, useId, useRef, useState } from "react";
import type { ProviderConnection, ProviderCredentialChange, ProviderSummary, ProviderType } from "@ez-assistant/protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { ModelFieldSelector } from "./ModelFieldSelector";
import { SettingsMessages } from "./SettingsMessages";
import { SettingsPageContainer } from "./SettingsPageContainer";
import { empty_connection, provider_labels, provider_defaults } from "./modelSettingsValues";
import styles from "./index.module.scss";

export const ProviderConnectionForm = observer(function ProviderConnectionForm(props: Readonly<{
  provider: ProviderSummary | null; onBack: () => void; onSaved: (provider: ProviderSummary) => void; onDirtyChange: (dirty: boolean) => void;
}>) {
  const settings = useRootStore().settings;
  const [connection, setConnection] = useState<ProviderConnection>(() => props.provider?.connection ?? empty_connection);
  const [secret, setSecret] = useState("");
  const [show_secret, setShowSecret] = useState(false);
  const secret_id = useId();
  const [clear_secret, setClearSecret] = useState(false);
  const [dirty, setDirty] = useState(false);
  useEffect(() => props.onDirtyChange(dirty), [dirty, props.onDirtyChange]);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const busy = settings.pending_action !== null;
  const responses = ["openai", "deepseek", "dashscope_api", "dashscope_plan", "moonshot"].includes(connection.provider_type);
  function change(patch: Partial<ProviderConnection>) { setConnection((current) => ({ ...current, ...patch })); setDirty(true); }
  async function save() {
    const credential: ProviderCredentialChange = clear_secret ? { mode: "clear" } : { mode: "unchanged" };
    const command: ProviderCredentialChange = secret ? { mode: "replace", value: secret } : credential;
    const saved = await settings.saveProvider(props.provider?.provider_instance_id ?? null, connection, command);
    if (!saved || !mounted.current) return;
    setSecret(""); setShowSecret(false); setDirty(false); props.onDirtyChange(false); props.onSaved(saved);
  }
  return <SettingsPageContainer title={props.provider ? "编辑服务商" : "添加服务商"} on_back={props.onBack}>
    <SettingsMessages />
    <form onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <fieldset disabled={busy} className={styles.model_fieldset}><div className={styles.model_form}>
        <label>名称<input placeholder="请输入" autoFocus aria-label="服务商名称" required maxLength={256} value={connection.display_name} onChange={(event) => change({ display_name: event.target.value })} /></label>
        <ModelFieldSelector disabled={busy} label="服务商类型" value={connection.provider_type} options={Object.entries(provider_labels).map(([value, label]) => ({ value: value as ProviderType, label }))} onChange={(provider_type) => change({ provider_type, ...provider_defaults[provider_type], display_name: !connection.display_name || connection.display_name === provider_labels[connection.provider_type] ? provider_labels[provider_type] : connection.display_name })} />
        <label>服务地址<input aria-label="服务地址" type="url" required maxLength={8192} placeholder="https://…/v1" value={connection.endpoint} onChange={(event) => change({ endpoint: event.target.value })} /></label>
        <div className={styles.form_field}><label htmlFor={secret_id}>API Key</label><span className={styles.secret_field}>
          <input id={secret_id} aria-label="API Key" type={show_secret ? "text" : "password"} autoComplete="off" maxLength={16384} value={secret} disabled={clear_secret} placeholder={props.provider?.has_api_key ? "已设置，留空保持" : "未设置"} onChange={(event) => { setSecret(event.target.value); setDirty(true); }} />
          <InlineIconButton className={styles.secret_toggle} icon={show_secret ? "eye-off" : "eye"}
            label={show_secret ? "隐藏本次输入" : "显示本次输入"} title={show_secret ? "隐藏本次输入" : "显示本次输入"}
            aria-controls={secret_id} aria-pressed={show_secret} disabled={busy || clear_secret} onClick={() => setShowSecret(!show_secret)} />
        </span></div>
      </div>
      {props.provider?.has_api_key && <div className={styles.runtime_actions}>
        <Button type="button" onClick={() => { setClearSecret(!clear_secret); setSecret(""); setShowSecret(false); setDirty(true); }}>{clear_secret ? "取消清除密钥" : "清除密钥"}</Button>
        {clear_secret && <span>保存后清除密钥</span>}
      </div>}
      <CollapsibleSection title="接口选项"><div className={styles.model_form}>
        <ModelFieldSelector disabled={busy} label="接口协议" value={connection.protocol_preference} options={[
          { value: "auto", label: "自动" }, { value: "chat_completions", label: "Chat Completions" }, ...(responses ? [{ value: "responses" as const, label: "Responses" }] : []),
        ]} onChange={(protocol_preference) => change({ protocol_preference })} />
        <label>模型列表接口<input aria-label="模型列表接口" value={connection.models_path} onChange={(event) => change({ models_path: event.target.value })} placeholder="留空使用适配器默认接口" /></label>

      </div></CollapsibleSection>
      <div className={styles.runtime_actions}><Button type="button" onClick={props.onBack} disabled={busy}>取消</Button><Button type="submit" disabled={busy}>{busy ? "正在保存…" : "保存服务商"}</Button></div>
    </fieldset></form>
  </SettingsPageContainer>;
});
