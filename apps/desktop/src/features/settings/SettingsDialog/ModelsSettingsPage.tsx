import { observer } from "mobx-react-lite";
import { useEffect, useState } from "react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../../components/DropdownMenu";
import { Icon } from "../../../components/Icon";
import { SelectionPopover, type SelectionOption } from "../../../components/SelectionPopover";
import type {
  ModelCatalogEntrySnapshot,
  ModelConfiguration,
  ModelConfigurationInput,
  ModelKey,
  SessionSummary,
} from "../../../generated/assistant-protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { ConflictDialog, DeleteModelDialog } from "./ModelSettingsDialogs";
import { SettingsMessages } from "./RuntimeSettingsPage";
import { SettingsPageContainer } from "./SettingsPageContainer";
import styles from "./index.module.scss";

type ModelDraft = {
  model_key: string;
  display_name: string;
  protocol: string;
  provider: string;
  endpoint: string;
  model: string;
  context_window_tokens: string;
  max_output_tokens: string;
  api_key: string;
  set_default: boolean;
};

const empty_draft: ModelDraft = {
  model_key: "",
  display_name: "",
  protocol: "openai_chat_completions",
  provider: "openai",
  endpoint: "",
  model: "",
  context_window_tokens: "128000",
  max_output_tokens: "8192",
  api_key: "",
  set_default: false,
};

export const ModelsSettingsPage = observer(function ModelsSettingsPage(props: Readonly<{
  onDirtyChange: (dirty: boolean) => void;
}>) {
  const settings = useRootStore().settings;
  const [editing, setEditing] = useState<ModelConfiguration | "new" | null>(null);
  const [draft, setDraft] = useState<ModelDraft>(empty_draft);
  const [dirty, setDirty] = useState(false);
  const [show_secret, setShowSecret] = useState(false);
  const [open_selector, setOpenSelector] = useState<"model" | "protocol" | "provider" | "vision" | null>(null);
  const [deleting, setDeleting] = useState<ModelConfiguration | null>(null);
  const [replacement_default, setReplacementDefault] = useState<ModelKey | "">("");
  const root_store = useRootStore();
  const active_sessions = root_store.projection.application?.active_sessions ?? [];

  useEffect(() => props.onDirtyChange(dirty), [dirty, props.onDirtyChange]);
  useEffect(() => () => props.onDirtyChange(false), [props.onDirtyChange]);

  function beginCreate() {
    settings.clearMessages();
    setEditing("new");
    setDraft(empty_draft);
    setDirty(false);
    setShowSecret(false);
  }

  function beginEdit(model: ModelConfiguration) {
    if (!model.model_key) return;
    settings.clearMessages();
    setEditing(model);
    setDraft({
      model_key: model.model_key,
      display_name: model.display_name,
      protocol: model.protocol ?? "openai_chat_completions",
      provider: model.provider ?? "",
      endpoint: model.endpoint ?? "",
      model: model.model ?? "",
      context_window_tokens: String(model.context_window_tokens ?? 128000),
      max_output_tokens: String(model.max_output_tokens ?? 8192),
      api_key: "",
      set_default: model.is_default,
    });
    setDirty(false);
    setShowSecret(false);
  }

  function cancelEdit() {
    if (dirty && !window.confirm("放弃尚未保存的修改吗？")) return;
    setEditing(null);
    setDraft(empty_draft);
    setDirty(false);
    setShowSecret(false);
    settings.clearMessages();
  }

  function update<K extends keyof ModelDraft>(key: K, value: ModelDraft[K]) {
    setDraft((current) => ({ ...current, [key]: value }));
    setDirty(true);
  }

  function candidate(): ModelConfigurationInput | null {
    const context_window_tokens = Number(draft.context_window_tokens);
    const max_output_tokens = Number(draft.max_output_tokens);
    const provider = draft.provider.trim();
    if (!draft.model_key.trim() || !draft.display_name.trim() || !draft.protocol
      || !provider || !draft.endpoint.trim() || !draft.model.trim()) {
      settings.showError("请填写显示名称、模型 Key、接口协议、供应商、模型 ID 和 Endpoint。");
      return null;
    }
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(draft.model_key.trim())) {
      settings.showError("模型 Key 只能使用字母、数字、点号、下划线和连字符，不能包含空格。");
      return null;
    }
    if (!/^[a-z0-9][a-z0-9_-]{0,63}$/.test(provider)) {
      settings.showError("供应商标识必须以小写字母或数字开头，只能使用小写字母、数字、下划线和连字符。");
      return null;
    }
    if (!Number.isSafeInteger(context_window_tokens) || context_window_tokens <= 0
      || !Number.isSafeInteger(max_output_tokens) || max_output_tokens <= 0) {
      settings.showError("上下文窗口和输出上限必须是正整数。");
      return null;
    }
    if (!isSafeEndpoint(draft.endpoint.trim())) {
      settings.showError("Endpoint 必须是无凭据、查询参数和片段的 HTTP 或 HTTPS 地址。");
      return null;
    }
    const is_new = editing === "new";
    if (is_new && !draft.api_key) {
      settings.showError("新增模型时必须填写 API Key。");
      return null;
    }
    return {
      model_key: draft.model_key.trim(),
      display_name: draft.display_name.trim(),
      protocol: draft.protocol,
      provider,
      endpoint: draft.endpoint.trim(),
      model: draft.model.trim(),
      context_window_tokens,
      max_output_tokens,
      credential: draft.api_key
        ? { mode: "replace", value: draft.api_key }
        : { mode: "unchanged" },
    };
  }

  async function save() {
    const model = candidate();
    if (!model) return;
    const success = editing === "new"
      ? await settings.createModel(model, draft.set_default)
      : await settings.updateModel(model, draft.set_default);
    if (success) {
      setEditing(null);
      setDraft(empty_draft);
      setDirty(false);
      setShowSecret(false);
    }
  }

  async function copyDraft() {
    await navigator.clipboard.writeText(JSON.stringify({
      model_key: draft.model_key,
      display_name: draft.display_name,
      protocol: draft.protocol,
      provider: draft.provider,
      endpoint: draft.endpoint,
      model: draft.model,
      context_window_tokens: draft.context_window_tokens,
      max_output_tokens: draft.max_output_tokens,
      api_key: draft.api_key || undefined,
      set_default: draft.set_default,
    }, null, 2));
    settings.showNotice("本次表单输入已复制。API Key 仅在你明确复制时写入剪贴板。");
  }

  async function reloadAfterConflict() {
    await settings.load();
    if (!settings.error_message) {
      setEditing(null);
      setDraft(empty_draft);
      setDirty(false);
      setShowSecret(false);
    }
  }

  async function testConnection() {
    const model = candidate();
    if (model) await settings.validateCandidate(model);
  }

  function beginDelete(model: ModelConfiguration) {
    if (!model.model_key) return;
    settings.clearMessages();
    const replacement = replacementCandidates(model)[0]?.model_key ?? "";
    setReplacementDefault(replacement ?? "");
    setDeleting(model);
  }

  async function confirmDelete() {
    if (!deleting?.model_key) return;
    const blockers = deletionBlockers(deleting, active_sessions);
    if (blockers.length) return;
    const replacement = deleting.is_default ? replacement_default || null : null;
    if (deleting.is_default && !replacement) {
      settings.showError("删除默认模型前，至少需要另一个有效模型作为替代。");
      return;
    }
    if (await settings.deleteModel(deleting.model_key, replacement)) {
      setDeleting(null);
      setReplacementDefault("");
    }
  }

  function replacementCandidates(model: ModelConfiguration) {
    return settings.models.filter((candidate) => (
      candidate.model_key
      && candidate.model_key !== model.model_key
      && candidate.is_valid
    ));
  }

  const auxiliary_vision_model = settings.status?.auxiliary_vision_model ?? "";
  const configured_vision_model_is_available = !auxiliary_vision_model || settings.models.some((model) => (
    model.model_key === auxiliary_vision_model
    && model.is_valid
    && model.supports_image_input
  ));
  const vision_model_options: readonly SelectionOption<string>[] = [
    {
      value: "",
      label: "未配置",
      description: "仅使用主模型的原生识图能力",
    },
    ...(configured_vision_model_is_available ? [] : [{
      value: auxiliary_vision_model,
      label: `当前配置（${auxiliary_vision_model}）`,
      description: "该模型不可用或不支持图片输入，请重新选择",
    }]),
    ...settings.models
      .filter((model) => model.model_key && model.is_valid && model.supports_image_input)
      .map((model) => ({
        value: model.model_key ?? "",
        label: model.display_name,
        description: `${model.provider ?? "未知供应商"} · ${model.model ?? model.model_key ?? "未知模型"}`,
      })),
  ];

  if (editing) {
    const is_new = editing === "new";
    const catalog_entries = settings.model_catalog?.entries ?? [];
    const protocol_options = catalogProtocolOptions(catalog_entries);
    const provider_options = catalogProviderOptions(catalog_entries, draft.protocol);
    const model_options = catalogModelOptions(catalog_entries, draft.protocol, draft.provider);
    const visible_protocol_options = includeCurrentOption(protocol_options, draft.protocol, "当前配置");
    return (
      <SettingsPageContainer
        back_label="返回模型列表"
        on_back={cancelEdit}
        title={is_new ? "添加模型" : "编辑模型"}
      >
        <div className={styles.model_form}>
          <label>显示名称<input onChange={(event) => update("display_name", event.currentTarget.value)} placeholder="例如：DeepSeek V4 Pro" value={draft.display_name} /></label>
          <label>模型 Key<input disabled={!is_new} onChange={(event) => update("model_key", event.currentTarget.value)} placeholder="应用内唯一标识，例如：deepseek-v4-pro" value={draft.model_key} /></label>
          <div className={styles.form_field}>
            接口协议（Protocol）
            <SelectionPopover
              aria_label="选择接口协议"
              content_width="content"
              on_open_change={(open) => setOpenSelector(open ? "protocol" : null)}
              on_select={(value) => update("protocol", value)}
              open={open_selector === "protocol"}
              options={visible_protocol_options}
              selected={draft.protocol}
              trigger_variant="field"
            />
          </div>
          <div className={styles.form_field}>
            供应商（Provider）
            <SelectionPopover
              aria_label="供应商（Provider）"
              content_width="content"
              editable
              on_open_change={(open) => setOpenSelector(open ? "provider" : null)}
              on_select={(value) => update("provider", value)}
              open={open_selector === "provider"}
              options={provider_options}
              placeholder="例如：deepseek"
              selected={draft.provider}
              trigger_variant="field"
            />
          </div>
          <div className={`${styles.form_field} ${styles.full_field}`}>
            模型 ID
            <SelectionPopover
              aria_label="模型 ID"
              content_width="content"
              editable
              on_open_change={(open) => setOpenSelector(open ? "model" : null)}
              on_select={(value) => update("model", value)}
              open={open_selector === "model"}
              options={model_options}
              placeholder="供应商接口使用的模型名，例如：deepseek-v4-pro"
              selected={draft.model}
              trigger_variant="field"
            />
          </div>
          <label className={styles.full_field}>Endpoint<input onChange={(event) => update("endpoint", event.currentTarget.value)} placeholder="接口地址，例如：http://127.0.0.1:8000/v1" value={draft.endpoint} /></label>
          <label className={`${styles.full_field} ${styles.secret_field}`}>API Key<span><input autoComplete="new-password" onChange={(event) => update("api_key", event.currentTarget.value)} placeholder={is_new ? "输入 API Key" : "留空则保持现有凭据"} type={show_secret ? "text" : "password"} value={draft.api_key} /><button aria-label={show_secret ? "隐藏 API Key" : "显示 API Key"} onClick={() => setShowSecret((visible) => !visible)} type="button">{show_secret ? "隐藏" : "显示"}</button></span></label>
          <label>上下文窗口<input inputMode="numeric" onChange={(event) => update("context_window_tokens", event.currentTarget.value)} placeholder="模型支持的 Token 数，例如：128000" value={draft.context_window_tokens} /></label>
          <label>单轮输出上限<input inputMode="numeric" onChange={(event) => update("max_output_tokens", event.currentTarget.value)} placeholder="单次最多输出的 Token 数，例如：8192" value={draft.max_output_tokens} /></label>
          <label className={styles.default_checkbox}><input checked={draft.set_default} onChange={(event) => update("set_default", event.currentTarget.checked)} type="checkbox" />设为默认模型</label>
        </div>
        <SettingsMessages />
        <footer className={styles.form_actions}>
          <button disabled={settings.pending_action !== null} onClick={() => void testConnection()} type="button">测试连接</button>
          <span />
          <button onClick={cancelEdit} type="button">取消</button>
          <button className={styles.primary_button} disabled={settings.pending_action !== null} onClick={() => void save()} type="button">保存</button>
        </footer>
        {settings.configuration_conflict && (
          <ConflictDialog
            onCopy={() => void copyDraft()}
            onReload={() => void reloadAfterConflict()}
          />
        )}
      </SettingsPageContainer>
    );
  }

  return (
    <SettingsPageContainer
      actions={(
        <button className={styles.primary_button} onClick={beginCreate} type="button"><Icon name="plus" size={14} />添加模型</button>
      )}
      title="模型"
    >
      <div className={styles.vision_model_setting}>
        <div className={styles.model_icon}><Icon name="model" size={17} /></div>
        <div className={styles.vision_model_summary}>
          <strong>默认识图模型</strong>
          <span>供不支持原生图片输入、但能够调用工具的主模型使用</span>
        </div>
        <div className={styles.vision_model_select}>
          <SelectionPopover
            aria_label="默认识图模型"
            content_width="content"
            disabled={settings.pending_action !== null}
            on_open_change={(open) => setOpenSelector(open ? "vision" : null)}
            on_select={(value) => void settings.setAuxiliaryVisionModel(value || null)}
            open={open_selector === "vision"}
            options={vision_model_options}
            selected={auxiliary_vision_model}
            trigger_variant="field"
          />
        </div>
      </div>
      <div className={styles.model_list}>
        {settings.models.map((model) => (
          <article data-valid={model.is_valid} key={model.model_key ?? model.display_name}>
            <div className={styles.model_icon}><Icon name="model" size={17} /></div>
            <div className={styles.model_summary}>
              <div className={styles.model_title}>
                <strong>{model.display_name}</strong>
                {model.is_default && <span className={styles.default_tag}>默认</span>}
              </div>
              <span>{model.provider ?? "配置无效"} · {model.model ?? model.model_key ?? "未知模型"} · {formatTokens(model.context_window_tokens)}</span>
              {!model.is_valid && <small>{model.issues[0]?.message ?? "模型配置无效"}</small>}
            </div>
            <div className={styles.model_actions}>
              <button disabled={!model.is_valid || settings.pending_action !== null || !model.model_key} onClick={() => model.model_key && void settings.validateConfigured(model.model_key)} type="button">测试连接</button>
              {model.model_key && (
                <DropdownMenu>
                  <DropdownMenuTrigger aria-label={`${model.display_name}的更多操作`} className={styles.more_button}>
                    <Icon name="more" size={16} />
                  </DropdownMenuTrigger>
                  <DropdownMenuContent aria-label={`${model.display_name}的模型操作`}>
                    <DropdownMenuItem onSelect={() => beginEdit(model)}>
                      编辑模型
                    </DropdownMenuItem>
                    {!model.is_default && model.is_valid && model.model_key && (
                      <DropdownMenuItem onSelect={() => {
                        if (model.model_key) void settings.setDefaultModel(model.model_key);
                      }}>
                        设为默认模型
                      </DropdownMenuItem>
                    )}
                    <DropdownMenuItem className={styles.danger_menu_item} onSelect={() => beginDelete(model)}>
                      删除模型
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              )}
            </div>
          </article>
        ))}
        {!settings.models.length && !settings.loading && <p className={styles.empty_models}>尚未配置模型。</p>}
      </div>
      <SettingsMessages />
      {deleting && (
        <DeleteModelDialog
          blockers={deletionBlockers(deleting, active_sessions)}
          model={deleting}
          onCancel={() => setDeleting(null)}
          onConfirm={() => void confirmDelete()}
          onReplacementChange={setReplacementDefault}
          pending={settings.pending_action === "delete"}
          replacement={replacement_default}
          replacements={replacementCandidates(deleting)}
        />
      )}
    </SettingsPageContainer>
  );
});

function deletionBlockers(model: ModelConfiguration, sessions: readonly SessionSummary[]): readonly SessionSummary[] {
  if (!model.model_key) return [];
  return sessions.filter((session) => (
    session.model_key === model.model_key
    && (session.active_run_id !== null || session.queued_input_count > 0)
  ));
}

function formatTokens(tokens: number | null): string {
  if (!tokens) return "—";
  return tokens >= 1_000 ? `${Math.round(tokens / 1_000)}K` : String(tokens);
}

function isSafeEndpoint(value: string): boolean {
  try {
    const endpoint = new URL(value);
    return (endpoint.protocol === "http:" || endpoint.protocol === "https:")
      && endpoint.username === ""
      && endpoint.password === ""
      && endpoint.search === ""
      && endpoint.hash === "";
  } catch {
    return false;
  }
}

function includeCurrentOption<T extends string>(
  options: readonly SelectionOption<T>[],
  current: string,
  current_label: string,
): readonly SelectionOption<string>[] {
  if (!current || options.some((option) => option.value === current)) {
    return options;
  }
  return [{ value: current, label: `${current_label}（${current}）` }, ...options];
}

function catalogProtocolOptions(
  entries: readonly ModelCatalogEntrySnapshot[],
): readonly SelectionOption<string>[] {
  return uniqueOptions(entries.map((entry) => ({
    value: entry.protocol,
    label: entry.protocol_label,
  })));
}

function catalogProviderOptions(
  entries: readonly ModelCatalogEntrySnapshot[],
  protocol: string,
): readonly SelectionOption<string>[] {
  return uniqueOptions(entries
    .filter((entry) => entry.protocol === protocol)
    .map((entry) => ({
      value: entry.provider,
      label: entry.provider_label,
      description: entry.provider,
    })));
}

function catalogModelOptions(
  entries: readonly ModelCatalogEntrySnapshot[],
  protocol: string,
  provider: string,
): readonly SelectionOption<string>[] {
  return uniqueOptions(entries
    .filter((entry) => entry.protocol === protocol && entry.provider === provider)
    .flatMap((entry) => entry.model_ids.map((model_id) => ({
      value: model_id,
      label: model_id,
    }))));
}

function uniqueOptions(
  options: readonly SelectionOption<string>[],
): readonly SelectionOption<string>[] {
  const seen = new Set<string>();
  return options.filter((option) => {
    if (seen.has(option.value)) return false;
    seen.add(option.value);
    return true;
  });
}
