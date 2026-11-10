import { Button } from '../../../components/Button';
import { observer } from 'mobx-react-lite';
import { useEffect, useRef, useState } from 'react';
import type {
  DiscoveredModel,
  ModelFixedConfig,
  ModelSelection,
  ModelSettings,
  ProviderInstanceId,
  ProviderSummary,
} from '@ez-assistant/protocol';
import { SettingsCascadePopover, type SettingsCascadeCategory, type SettingsCascadeOption } from '../../../components/SettingsCascadePopover';
import { useRootStore } from '../../../stores/RootStoreContext';

export type ModelPickerProps = Readonly<{
  source?: "conversation" | "settings";
  providers: readonly ProviderSummary[];
  selection: ModelSelection | null;
  title: string;
  label: string;
  open: boolean;
  disabled?: boolean;
  disabled_reason?: string;
  onOpenChange: (open: boolean) => void;
  onSelect: (selection: ModelSelection | null) => Promise<boolean>;
  trigger_class_name: string;
  follow_default?: boolean;
  vision_only?: boolean;
  clearable?: boolean;
  show_manage_providers?: boolean;
  additional_categories?: readonly SettingsCascadeCategory[];
  initial_category?: string | null;
}>;

/** 只保留正在展开服务商的本地目录快照；跨用途共用相同的二级选择路径。 */
export const ModelPicker = observer(function ModelPicker(props: ModelPickerProps) {
  const root = useRootStore();
  const settings = root.settings;
  const remote = props.source === "conversation" && root.remote_device;
  const owner = props.source === 'conversation' ? root.runtime_client : root.local_runtime_client;
  const client = remote ? owner : null;
  const model_settings = props.source === 'conversation' ? root.projection.application?.model_settings : settings.model_settings;
  const management = model_settings?.management;
  const selection = props.selection;
  const [result, setResult] = useState<Readonly<{
    // 同一服务商标识可能出现在不同用户域，连接身份也是目录结果的归属。
    owner: typeof owner;
    provider_id: ProviderInstanceId;
    source: ProviderSummary["source"];
    models: readonly DiscoveredModel[];
    fixed: readonly ModelFixedConfig[];
    loading: boolean;
    fixed_loading: boolean;
    error: string | null;
    fixed_error: string | null;
  }> | null>(null);
  const request = useRef<AbortController | null>(null);
  useEffect(() => {
    if (!props.open) {
      request.current?.abort();
      setResult(null);
    }
    return () => request.current?.abort();
  }, [props.open, owner]);
  async function load(provider_id: ProviderInstanceId, source: ProviderSummary["source"]) {
    request.current?.abort();
    const current = new AbortController();
    request.current = current;
    setResult({
      owner,
      provider_id,
      source,
      models: [],
      fixed: [],
      loading: true,
      fixed_loading: true,
      error: null,
      fixed_error: null,
    });
    // 两种来源独立加载，慢目录和失败目录不阻塞持久化模型。
    void (async () => {
      try {
        const models = client ? (await client.command({ type: "list_provider_models", payload: { source, provider_instance_id: provider_id } }, { signal: current.signal })).payload.models
          : await settings.listProviderModels(provider_id, current.signal, source);
        if (!current.signal.aborted) setResult((value) => (value ? { ...value, models, loading: false } : value));
      } catch (failure: unknown) {
        if (!current.signal.aborted)
          setResult((value) =>
            value
              ? { ...value, loading: false, error: failure instanceof Error ? failure.message : '读取模型列表失败。' }
              : value,
          );
      }
    })();
    try {
      const fixed: ModelFixedConfig[] = [];
      if (client) {
        for (let offset = 0; !current.signal.aborted; offset += 20) {
          const page = (await client.command({ type: "list_fixed_model_configs", payload: { source, provider_instance_id: provider_id, offset, limit: 20 } }, { signal: current.signal })).payload;
          fixed.push(...page);
          if (page.length < 20) break;
        }
      } else fixed.push(...await settings.listAllFixedModels(provider_id, current.signal, source));
      if (!current.signal.aborted) setResult((value) => (value ? { ...value, fixed, fixed_loading: false } : value));
    } catch (failure: unknown) {
      if (!current.signal.aborted)
        setResult((value) =>
          value
            ? {
                ...value,
                fixed_loading: false,
                fixed_error: failure instanceof Error ? failure.message : '读取已保存模型失败。',
              }
            : value,
        );
    }
  }

  const organization = props.providers.filter((provider) => provider.source === 'organization');
  const personal = props.providers.filter((provider) => provider.source !== 'organization');
  const grouped = organization.length > 0 && personal.length > 0;
  const categories: SettingsCascadeCategory[] = [...organization, ...personal].map((provider, index) => {
    const id = provider.provider_instance_id;
    const source = provider.source ?? "local";
    const source_label = source === 'organization' ? '企业模型' : '个人模型';
    const current = result?.owner === owner && result.provider_id === id && result.source === source ? result : null;
    const models = new Map<string, SettingsCascadeOption>(
      (current?.models ?? []).map((model) => [
        model.model_id,
        {
          value: model.model_id,
          label: model.display_name ?? model.model_id,
          description: model.display_name && model.display_name !== model.model_id ? model.model_id : undefined,
          disabled: props.vision_only && model.metadata.image_input === 'unsupported',
        },
      ]),
    );
    for (const fixed of current?.fixed ?? []) {
      const id = fixed.selection.model_id;
      const online = models.get(id);
      models.set(id, {
        value: id,
        label: online?.label ?? id,
        description: online?.description,
        disabled: props.vision_only && fixed.parameters.image_input !== 'supported',
      });
    }
    return {
      id: `${source}:${id}`,
      label: provider.connection.display_name,
      heading_before: grouped && (index === 0 || index === organization.length) ? source_label : undefined,
      accessible_description: grouped ? source_label : undefined,
      separator_before: grouped && index === organization.length,
      value_label: '',
      hide_title: true,
      selected: selection?.provider_instance_id === id && (selection.source ?? 'local') === source ? selection.model_id : '',
      options: [...models.values()].map((option) =>
        option.disabled ? { ...option, description: '需要支持图片输入' } : option,
      ),
      on_open: () => {
        void load(id, source);
      },
      on_select: (model_id) => props.onSelect({ source, provider_instance_id: id, model_id }),
      content:
        current &&
        (current.loading || current.fixed_loading || current.error || current.fixed_error || models.size === 0) ? (
          <>
            {current.loading && <p role="status">正在读取模型目录…</p>}
            {current.fixed_loading && <p role="status">正在读取已保存模型…</p>}
            {current?.fixed_error && (
              <p role="alert">
                {current.fixed_error}
                <Button type="button" onClick={() => void load(id, source)}>
                  重试读取
                </Button>
              </p>
            )}
            {current?.error && (
              <p role="alert">
                {current.error}
                <Button type="button" onClick={() => void load(id, source)}>
                  重试
                </Button>
              </p>
            )}
            {current &&
              !current.loading &&
              !current.fixed_loading &&
              !current.error &&
              !current.fixed_error &&
              models.size === 0 && <p>暂无可用模型，请到模型管理页刷新在线模型。</p>}
          </>
        ) : null,
    };
  });
  categories.push(...(props.additional_categories ?? []));
  const unavailable = modelSelectionIssue(selection, props.providers, model_settings)
    ?? (props.follow_default ? management?.unavailable_reason : null);
  const refresh_error = management?.last_refresh_error;
  return (
    <SettingsCascadePopover
      aria_label={props.title}
      open={props.open}
      on_open_change={props.onOpenChange}
      clear_action={
        props.clearable && props.selection
          ? { label: `清除${props.title}`, on_clear: () => props.onSelect(null) }
          : undefined
      }
      disabled={props.disabled}
      disabled_reason={props.disabled_reason}
      initial_category={props.initial_category ?? null}
      categories={categories}
      trigger_class_name={props.trigger_class_name}
      trigger_content={props.label}
      primary_actions={
        props.follow_default
          ? [
              {
                label: management ? '跟随组织默认' : '默认模型',
                selected: props.selection === null,
                on_select: () => props.onSelect(null),
              },
            ]
          : []
      }
      primary_content={
        <>
          {unavailable && <p role="status">{unavailable}</p>}
          {refresh_error && <p role="alert">企业模型刷新失败：{refresh_error}</p>}
          {props.providers.length === 0 && !refresh_error && <p>暂无服务商</p>}
        </>
      }
      footer_content={
        !remote && props.show_manage_providers !== false && (
          <Button
            type="button"
            role="menuitem"
            onClick={() => {
              props.onOpenChange(false);
              settings.open('models');
            }}
          >
            管理服务商
          </Button>
        )
      }
    />
  );
});

export function modelSelectionIssue(
  selection: ModelSelection | null,
  providers: readonly ProviderSummary[],
  settings: ModelSettings | undefined,
): string | undefined {
  if (!selection || !settings) return undefined;
  const provider = providers.find((item) => matchesProvider(item, selection));
  if (selection.source === 'organization') {
    const unavailable = settings.management?.unavailable_models?.find(({ selection: item }) =>
      item.provider_instance_id === selection.provider_instance_id && item.model_id === selection.model_id,
    );
    if (unavailable) return unavailable.reason;
    if (!provider) return '当前组织未提供此模型，请重新选择。';
    const default_model = settings.default_model;
    if (settings.management && !settings.management.allow_member_selection &&
      (default_model?.provider_instance_id !== selection.provider_instance_id || default_model.model_id !== selection.model_id)) {
      return '当前组织仅提供默认企业模型，请重新选择。';
    }
  }
  if (!provider) return '所选服务商不可用，请重新选择。';
  return undefined;
}

export function modelSelectionLabel(
  selection: ModelSelection,
  providers: readonly ProviderSummary[],
  enterprise: boolean,
): string {
  const provider = providers.find((item) => matchesProvider(item, selection));
  const prefix = enterprise ? (selection.source === 'organization' ? '企业模型 · ' : '个人模型 · ') : '';
  const name = provider?.connection.display_name ?? (enterprise || selection.source === 'organization' ? '服务商不可用' : '服务商已删除');
  return `${prefix}${name} / ${selection.model_id}`;
}

function matchesProvider(provider: ProviderSummary, selection: ModelSelection): boolean {
  return provider.provider_instance_id === selection.provider_instance_id &&
    (provider.source ?? 'local') === (selection.source ?? 'local');
}
