import type {
  ModelKey,
  ReasoningEffortKey,
  ReasoningEffortOptionSnapshot,
} from "../../../generated/assistant-protocol";
import {
  SettingsCascadePopover,
  type SettingsCascadeCategory,
  type SettingsCascadeOption,
} from "./SettingsCascadePopover";

type ModelOption = Readonly<{
  description: string;
  label: string;
  value: ModelKey;
}>;

type ModelSettingsPopoverProps = Readonly<{
  disabled: boolean;
  effort: ReasoningEffortKey | null;
  effort_options: readonly ReasoningEffortOptionSnapshot[];
  initial_category: "model" | "effort" | null;
  model_display_name: string;
  model_key: ModelKey;
  model_options: readonly ModelOption[];
  model_switch_disabled_reason?: string;
  on_effort_change: (effort: ReasoningEffortKey | null) => Promise<boolean>;
  on_model_change: (model_key: ModelKey) => Promise<boolean>;
  on_open_change: (open: boolean) => void;
  open: boolean;
  trigger_class_name: string;
}>;

export function ModelSettingsPopover(props: ModelSettingsPopoverProps) {
  const categories: SettingsCascadeCategory[] = [{
    id: "model",
    label: "模型",
    selected: props.model_key,
    value_label: props.model_display_name,
    disabled_reason: props.model_switch_disabled_reason,
    options: props.model_options,
    on_select: (value) => props.on_model_change(value),
  }];
  if (props.effort_options.length > 0) {
    const options: SettingsCascadeOption[] = [
      { value: "default", label: "默认", description: "使用当前模型的默认思考强度" },
      ...props.effort_options.map((option) => ({ value: option.key, label: option.label })),
    ];
    categories.push({
      id: "effort",
      label: "推理强度",
      selected: props.effort ?? "default",
      value_label: effortLabel(props.effort, props.effort_options),
      options,
      on_select: (value) => props.on_effort_change(
        value === "default" ? null : value as ReasoningEffortKey,
      ),
    });
  }
  return (
    <SettingsCascadePopover
      aria_label="模型设置"
      categories={categories}
      disabled={props.disabled}
      initial_category={props.initial_category}
      on_open_change={props.on_open_change}
      open={props.open}
      trigger_class_name={props.trigger_class_name}
      trigger_content={`${props.model_display_name} · ${effortLabel(props.effort, props.effort_options)}`}
    />
  );
}

function effortLabel(
  effort: ReasoningEffortKey | null,
  options: readonly ReasoningEffortOptionSnapshot[],
): string {
  if (!effort) return "默认";
  return options.find((option) => option.key === effort)?.label ?? effort;
}
