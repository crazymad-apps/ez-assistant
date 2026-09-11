import { Button } from "../../../components/Button";
import { useEffect, useRef, useState } from "react";
import type { DiscoveredModel, ModelFixedConfig, ModelSelection, ProviderInstanceId, ProviderSummary } from "@ez-assistant/protocol";
import { SettingsCascadePopover, type SettingsCascadeCategory } from "../../../components/SettingsCascadePopover";
import { useRootStore } from "../../../stores/RootStoreContext";

export type ModelPickerProps = Readonly<{
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
  clearable?: boolean;
  show_manage_providers?: boolean;
  additional_categories?: readonly SettingsCascadeCategory[];
  initial_category?: string | null;
}>;

/** 只保留正在展开的服务商本次在线结果；跨用途共用相同的二级选择路径。 */
export function ModelPicker(props: ModelPickerProps) {
  const settings = useRootStore().settings;
  const [result, setResult] = useState<Readonly<{
    provider_id: ProviderInstanceId; models: readonly DiscoveredModel[]; fixed: readonly ModelFixedConfig[]; loading: boolean; fixed_loading: boolean; error: string | null; fixed_error: string | null;
  }> | null>(null);
  const request = useRef<AbortController | null>(null);
  useEffect(() => {
    if (!props.open) { request.current?.abort(); setResult(null); }
    return () => request.current?.abort();
  }, [props.open]);
  async function load(provider_id: ProviderInstanceId) {
    request.current?.abort();
    const current = new AbortController(); request.current = current;
    setResult({ provider_id, models: [], fixed: [], loading: true, fixed_loading: true, error: null, fixed_error: null });
    // 两种来源独立加载，慢目录和失败目录不阻塞持久化模型。
    void (async () => {
      try {
        const models = await settings.listProviderModels(provider_id, current.signal);
        if (!current.signal.aborted) setResult((value) => value ? { ...value, models, loading: false } : value);
      } catch (failure: unknown) {
        if (!current.signal.aborted) setResult((value) => value ? { ...value, loading: false, error: failure instanceof Error ? failure.message : "获取模型失败。" } : value);
      }
    })();
    try {
      const fixed = await settings.listAllFixedModels(provider_id, current.signal);
      if (!current.signal.aborted) setResult((value) => value ? { ...value, fixed, fixed_loading: false } : value);
    } catch (failure: unknown) {
      if (!current.signal.aborted) setResult((value) => value ? { ...value, fixed_loading: false, fixed_error: failure instanceof Error ? failure.message : "读取已保存模型失败。" } : value);
    }
  }

  const categories: SettingsCascadeCategory[] = props.providers.map((provider) => {
    const id = provider.provider_instance_id;
    const current = result?.provider_id === id ? result : null;
    const models = new Map((current?.models ?? []).map((model) => [model.model_id, {
      value: model.model_id, label: model.display_name ?? model.model_id,
      description: model.display_name && model.display_name !== model.model_id ? model.model_id : undefined,
    }]));
    for (const fixed of current?.fixed ?? []) {
      const id = fixed.selection.model_id;
      const online = models.get(id);
      models.set(id, { value: id, label: online?.label ?? id,
        description: online?.description });
    }
    return {
      id, label: provider.connection.display_name, value_label: "",
      hide_title: true,
      disabled_reason: props.disabled_reason,
      selected: props.selection?.provider_instance_id === id ? props.selection.model_id : "",
      options: [...models.values()],
      on_open: () => { void load(id); },
      on_select: (model_id) => props.onSelect({ provider_instance_id: id, model_id }),
      content: current && (current.loading || current.fixed_loading || current.error || current.fixed_error || models.size === 0) ? <>
        {current?.loading && <p role="status">正在获取在线模型…</p>}
        {current?.fixed_loading && <p role="status">正在读取已保存模型…</p>}
        {current?.fixed_error && <p role="alert">{current.fixed_error}<Button type="button" onClick={() => void load(id)}>重试读取</Button></p>}
        {current?.error && <p role="alert">{current.error}<Button type="button" onClick={() => void load(id)}>重试</Button></p>}
        {current && !current.loading && !current.fixed_loading && !current.error && !current.fixed_error && models.size === 0 && <p>本次列表没有匹配的模型。</p>}
      </> : null,
    };
  });
  categories.push(...props.additional_categories ?? []);
  return <SettingsCascadePopover aria_label={props.title} open={props.open} on_open_change={props.onOpenChange}
    clear_action={props.clearable && props.selection ? { label: `清除${props.title}`, on_clear: () => props.onSelect(null) } : undefined}
    disabled={props.disabled} initial_category={props.initial_category ?? null} categories={categories}
    trigger_class_name={props.trigger_class_name} trigger_content={props.label}
    primary_actions={props.follow_default ? [{
      label: "默认模型",
      selected: props.selection === null,
      disabled: Boolean(props.disabled_reason),
      on_select: () => props.onSelect(null),
    }] : []}
    primary_content={<>
      {props.providers.length === 0 && <p>暂无服务商</p>}
    </>} footer_content={props.show_manage_providers !== false && <Button type="button" role="menuitem" onClick={() => { props.onOpenChange(false); settings.open("models"); }}>管理服务商</Button>} />;
}
