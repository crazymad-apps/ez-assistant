import { serverHistorySupport } from "@ez-assistant/protocol";
import { observer } from "mobx-react-lite";
import { useEffect, useId, useRef, useState } from "react";
import type {
  ProviderConnection,
  ProviderCredentialChange,
  ProviderSummary,
  ProviderType,
  TestProviderConnectionResult,
} from "@ez-assistant/protocol";
import { Button } from "../../../components/Button";
import { CollapsibleSection } from "../../../components/CollapsibleSection";
import { InlineIconButton } from "../../../components/InlineIconButton";
import { ProviderIcon, inferModelProviderIcon } from "../../../components/ProviderIcon";
import { useRootStore } from "../../../stores/RootStoreContext";
import { ModelFieldSelector } from "./ModelFieldSelector";
import { OpenCodeConnectionFields } from "./OpenCodeConnectionFields";
import { SettingsMessages } from "./SettingsMessages";
import { SettingsPageContainer } from "./SettingsPageContainer";
import {
  empty_connection,
  provider_default_endpoints,
  provider_defaults,
  provider_labels,
  provider_preset_categories,
  provider_quick_presets,
  type ProviderQuickPreset,
} from "./modelSettingsValues";
import styles from "./index.module.scss";

function presetToConnection(preset: ProviderQuickPreset): ProviderConnection {
  const defaults = provider_defaults[preset.provider_type];
  return {
    ...empty_connection,
    display_name: preset.display_name,
    provider_type: preset.provider_type,
    endpoint: preset.endpoint,
    models_path: preset.models_path ?? "",
    server_history_support_override: null,
    discovery_format: defaults.discovery_format,
    protocol_preference: preset.protocol_preference ?? defaults.protocol_preference,
  };
}

function presetMatchesQuery(preset: ProviderQuickPreset, trimmed_query: string): boolean {
  if (!trimmed_query) return true;
  const haystacks = [preset.label, preset.display_name, preset.id, ...preset.keywords];
  return haystacks.some((text) => text.toLowerCase().includes(trimmed_query));
}

export const ProviderConnectionForm = observer(function ProviderConnectionForm(props: Readonly<{
  provider: ProviderSummary | null;
  onBack: () => void;
  onSaved: (provider: ProviderSummary) => void;
  onDirtyChange: (dirty: boolean) => void;
}>) {
  const settings = useRootStore().settings;
  const initial_preset = provider_quick_presets[0];
  const [connection, setConnection] = useState<ProviderConnection>(
    () => props.provider?.connection ?? presetToConnection(initial_preset),
  );
  const [selected_preset_id, setSelectedPresetId] = useState<string>(() => initial_preset.id);
  const [provider_query, setProviderQuery] = useState("");
  const [advanced_open, setAdvancedOpen] = useState(false);
  const [secret, setSecret] = useState("");
  const [show_secret, setShowSecret] = useState(false);
  const secret_id = useId();
  const [clear_secret, setClearSecret] = useState(false);
  const [test_result, setTestResult] = useState<TestProviderConnectionResult | null>(null);
  const [dirty, setDirty] = useState(false);
  useEffect(() => props.onDirtyChange(dirty), [dirty, props.onDirtyChange]);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const is_create = props.provider === null;
  const busy = settings.pending_action !== null;
  const testing = settings.pending_action === "provider:test";
  const saving = settings.pending_action === "provider:save";
  const opencode = connection.provider_type === "opencode";
  const responses = [
    "opencode",
    "openai",
    "sub2api",
    "deepseek",
    "dashscope_api",
    "dashscope_plan",
    "moonshot",
  ].includes(connection.provider_type);
  const history_support = serverHistorySupport(connection);
  let history_reason = "当前接口不支持服务端历史续接。";
  if (history_support === "unknown") history_reason = "尚未确认当前接口支持续接，可在接口选项中声明能力。";
  if (opencode) history_reason = "OpenCode 暂未开放服务端历史续接。";

  function change(patch: Partial<ProviderConnection>) {
    setConnection((current) => {
      const next = { ...current, ...patch };
      if (serverHistorySupport(next) !== "supported") next.server_history_enabled = false;
      return next;
    });
    setTestResult(null);
    setDirty(true);
  }

  function buildCredentialChange(): ProviderCredentialChange {
    const credential: ProviderCredentialChange = clear_secret ? { mode: "clear" } : { mode: "unchanged" };
    return secret ? { mode: "replace", value: secret } : credential;
  }

  function selectPreset(preset: ProviderQuickPreset) {
    setSelectedPresetId(preset.id);
    setAdvancedOpen(Boolean(preset.requires_custom_endpoint));
    change(presetToConnection(preset));
    document.getElementById(secret_id)?.focus();
  }

  function changeProviderType(provider_type: ProviderType) {
    const current_default = provider_default_endpoints[connection.provider_type];
    const next_default = provider_default_endpoints[provider_type];
    const auto_endpoint = is_create && (!connection.endpoint.trim() || connection.endpoint === current_default)
      ? next_default
      : connection.endpoint;
    const next_name = !connection.display_name || connection.display_name === provider_labels[connection.provider_type]
      ? provider_labels[provider_type]
      : connection.display_name;
    change({
      provider_type,
      endpoint: auto_endpoint,
      server_history_support_override: null,
      ...provider_defaults[provider_type],
      display_name: next_name,
    });
  }

  async function testConnection() {
    if (!connection.endpoint.trim()) {
      if (is_create) setAdvancedOpen(true);
      settings.showError("请先填写服务地址。");
      return;
    }
    const result = await settings.testProviderConnection({
      ...(props.provider ? { provider_instance_id: props.provider.provider_instance_id } : {}),
      connection,
      credential: buildCredentialChange(),
    });
    if (result && mounted.current) setTestResult(result);
  }

  async function save() {
    if (!connection.display_name.trim() || !connection.endpoint.trim()) {
      if (is_create) setAdvancedOpen(true);
      settings.showError("请填写服务商名称和服务地址。");
      return;
    }
    const saved = await settings.saveProvider(
      props.provider?.provider_instance_id ?? null,
      connection,
      buildCredentialChange(),
    );
    if (!saved || !mounted.current) return;
    setSecret("");
    setShowSecret(false);
    setTestResult(null);
    setDirty(false);
    props.onDirtyChange(false);
    props.onSaved(saved);
  }

  const matched_preset = is_create
    ? provider_quick_presets.find((item) => item.id === selected_preset_id)
    : undefined;
  const preset_active = Boolean(
    matched_preset
      && connection.provider_type === matched_preset.provider_type
      && connection.endpoint === matched_preset.endpoint,
  );
  const hero_icon = preset_active && matched_preset ? matched_preset.icon : inferModelProviderIcon(connection);
  const trimmed_query = provider_query.trim().toLowerCase();
  const filtered_groups = provider_preset_categories
    .map((category) => ({
      category,
      items: provider_quick_presets.filter(
        (preset) => preset.category === category.id && presetMatchesQuery(preset, trimmed_query),
      ),
    }))
    .filter((group) => group.items.length > 0);

  let create_placeholder = "请输入 API Key";
  if (props.provider) {
    create_placeholder = props.provider.has_api_key ? "已设置，留空保持" : "未设置";
  } else if (preset_active && matched_preset?.api_key_placeholder) {
    create_placeholder = matched_preset.api_key_placeholder;
  }

  const secret_field = (
    <div className={styles.form_field}>
      <label htmlFor={secret_id}>API Key</label>
      <span className={styles.secret_field}>
        <input
          id={secret_id}
          aria-label="API Key"
          autoFocus={is_create}
          type={show_secret ? "text" : "password"}
          autoComplete="off"
          maxLength={16384}
          value={secret}
          disabled={clear_secret}
          placeholder={create_placeholder}
          onChange={(event) => {
            setSecret(event.target.value);
            setTestResult(null);
            setDirty(true);
          }}
        />
        <InlineIconButton
          className={styles.secret_toggle}
          icon={show_secret ? "eye-off" : "eye"}
          label={show_secret ? "隐藏本次输入" : "显示本次输入"}
          title={show_secret ? "隐藏本次输入" : "显示本次输入"}
          aria-controls={secret_id}
          aria-pressed={show_secret}
          disabled={busy || clear_secret}
          onClick={() => setShowSecret(!show_secret)}
        />
      </span>
    </div>
  );

  const protocol_fields = (
    <>
      <ModelFieldSelector
        disabled={busy}
        label="接口协议"
        value={connection.protocol_preference}
        options={[
          ...(!opencode ? [{ value: "auto" as const, label: "自动" }] : []),
          { value: "chat_completions", label: "Chat Completions" },
          ...(responses ? [{ value: "responses" as const, label: "Responses" }] : []),
        ]}
        onChange={(protocol_preference) => change({ protocol_preference })}
      />
      <ModelFieldSelector
        disabled={busy || opencode}
        label="续接能力"
        value={connection.server_history_support_override ?? "default"}
        options={[
          { value: "default", label: "使用服务商默认" },
          { value: "supported", label: "明确支持" },
          { value: "unsupported", label: "明确不支持" },
        ]}
        onChange={(value) => change({ server_history_support_override: value === "default" ? null : value })}
      />
      <label>
        模型列表接口
        <input
          aria-label="模型列表接口"
          value={connection.models_path}
          onChange={(event) => change({ models_path: event.target.value })}
          placeholder="留空使用适配器默认接口"
        />
      </label>
    </>
  );

  const provider_identity_fields = (
    <>
      <label>
        名称
        <input
          placeholder="请输入"
          autoFocus={!is_create}
          aria-label="服务商名称"
          required
          maxLength={256}
          value={connection.display_name}
          onChange={(event) => change({ display_name: event.target.value })}
        />
      </label>
      <ModelFieldSelector
        disabled={busy}
        label="服务商类型"
        value={connection.provider_type}
        options={Object.entries(provider_labels).map(([value, label]) => ({
          value: value as ProviderType,
          label,
        }))}
        onChange={changeProviderType}
      />
      <label>
        服务地址
        <input
          aria-label="服务地址"
          type="url"
          required
          maxLength={8192}
          placeholder="https://…/v1"
          value={connection.endpoint}
          onChange={(event) => change({ endpoint: event.target.value })}
        />
      </label>
    </>
  );

  const advanced_notes_and_history = (
    <>
      {opencode && <OpenCodeConnectionFields connection={connection} onChange={change} />}
      {connection.provider_type === "sub2api" && (
        <p>自动获取增强模型目录，不支持时读取普通列表；无需填写查询参数，缺失能力保持未知。</p>
      )}
      {props.provider?.has_api_key && (
        <div className={styles.runtime_actions}>
          <Button
            type="button"
            onClick={() => {
              setClearSecret(!clear_secret);
              setSecret("");
              setShowSecret(false);
              setTestResult(null);
              setDirty(true);
            }}
          >
            {clear_secret ? "取消清除密钥" : "清除密钥"}
          </Button>
          {clear_secret && <span>保存后清除密钥</span>}
        </div>
      )}
      <div className={styles.history_policy}>
        <label className={styles.switch}>
          <input
            aria-label="使用服务端历史续接"
            type="checkbox"
            checked={connection.server_history_enabled}
            disabled={busy || history_support !== "supported"}
            onChange={(event) => change({ server_history_enabled: event.target.checked })}
          />
          <span />
          <b>使用服务端历史续接</b>
        </label>
        <p>
          {history_support === "supported"
            ? "开启后由服务商保存响应，用于后续增量续接；本地仍保留会话。"
            : history_reason}
        </p>
      </div>
    </>
  );

  const action_footer = (
    <>
      {test_result && (
        <div className={styles.provider_test_summary}>
          <strong>连接可用（耗时 {test_result.elapsed_ms} ms），共发现 {test_result.model_count} 个模型</strong>
          {test_result.sample_model_ids.length > 0 && (
            <span>示例模型：{test_result.sample_model_ids.join("、")}</span>
          )}
        </div>
      )}
      <div className={styles.runtime_actions}>
        <Button type="button" onClick={props.onBack} disabled={busy}>取消</Button>
        <Button type="button" onClick={() => void testConnection()} disabled={busy || !connection.endpoint.trim()}>
          {testing ? "正在测试…" : "测试连接"}
        </Button>
        <Button type="submit" disabled={busy}>{saving ? "正在保存…" : "保存服务商"}</Button>
      </div>
    </>
  );

  return (
    <SettingsPageContainer title={props.provider ? "编辑服务商" : "添加服务商"} on_back={props.onBack}>
      <SettingsMessages />
      <form onSubmit={(event) => { event.preventDefault(); void save(); }}>
        <fieldset disabled={busy} className={styles.model_fieldset}>
          {is_create ? (
            <div className={styles.provider_setup_layout}>
              <aside className={styles.provider_sidebar} role="group" aria-label="选择服务商">
                <input
                  className={styles.provider_sidebar_search}
                  aria-label="搜索服务商"
                  placeholder="搜索服务商…"
                  value={provider_query}
                  onChange={(event) => setProviderQuery(event.target.value)}
                />
                <div className={styles.provider_sidebar_list}>
                  {filtered_groups.length > 0 ? filtered_groups.map(({ category, items }) => (
                    <div key={category.id} className={styles.provider_sidebar_group}>
                      <span className={styles.provider_sidebar_group_label}>{category.label}</span>
                      {items.map((preset) => {
                        const active = selected_preset_id === preset.id;
                        return (
                          <button
                            key={preset.id}
                            type="button"
                            className={styles.provider_sidebar_item}
                            data-active={active ? "true" : "false"}
                            onClick={() => selectPreset(preset)}
                          >
                            <ProviderIcon name={preset.icon} size={18} />
                            <span>{preset.label}</span>
                          </button>
                        );
                      })}
                    </div>
                  )) : (
                    <p className={styles.provider_sidebar_empty}>
                      没有匹配的服务商，清空搜索或选择“自定义 OpenAI 兼容”。
                    </p>
                  )}
                </div>
              </aside>
              <div className={styles.provider_setup_main}>
                <div className={styles.provider_hero}>
                  <ProviderIcon name={hero_icon} size={28} />
                  <div className={styles.provider_hero_meta}>
                    <strong>{connection.display_name.trim() || matched_preset?.label || "自定义服务商"}</strong>
                    <span>{preset_active && matched_preset ? matched_preset.hint : "已自定义连接参数，可在下方“高级设置”继续调整。"}</span>
                    <small className={styles.provider_hero_endpoint}>
                      {connection.endpoint.trim() || "请在下方“高级设置”中填写服务地址"}
                    </small>
                  </div>
                </div>
                {secret_field}
                {action_footer}
                <CollapsibleSection title="高级设置" open={advanced_open} onOpenChange={setAdvancedOpen}>
                  <div className={styles.model_form}>
                    {provider_identity_fields}
                    {protocol_fields}
                  </div>
                  {advanced_notes_and_history}
                </CollapsibleSection>
              </div>
            </div>
          ) : (
            <>
              <div className={styles.provider_hero}>
                <ProviderIcon name={inferModelProviderIcon(connection)} size={28} />
                <div className={styles.provider_hero_meta}>
                  <strong>{connection.display_name.trim() || provider_labels[connection.provider_type]}</strong>
                  <span>{provider_labels[connection.provider_type]}</span>
                </div>
              </div>
              <div className={styles.model_form}>
                {provider_identity_fields}
                {secret_field}
              </div>
              {advanced_notes_and_history}
              <CollapsibleSection title="接口选项">
                <div className={styles.model_form}>
                  {protocol_fields}
                </div>
              </CollapsibleSection>
              {action_footer}
            </>
          )}
        </fieldset>
      </form>
    </SettingsPageContainer>
  );
});
