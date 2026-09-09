import type {
  ModelSelection,
  ProviderSummary,
  ReasoningEffortKey,
  ReasoningEffortOptionSnapshot,
} from "../../../generated/assistant-protocol";
import {
  type SettingsCascadeCategory,
  type SettingsCascadeOption,
} from "../../../components/SettingsCascadePopover";

import { ModelPicker } from "../../settings";

type ModelSettingsPopoverProps = Readonly<{
  disabled: boolean;
  effort: ReasoningEffortKey | null;
  effort_options: readonly ReasoningEffortOptionSnapshot[];
  initial_category: "model" | "effort" | null;
  model_display_name: string;
  selection: ModelSelection | null;
  providers: readonly ProviderSummary[];
  model_switch_disabled_reason?: string;
  on_effort_change: (effort: ReasoningEffortKey | null) => Promise<boolean>;
  on_model_change: (selection: ModelSelection | null) => Promise<boolean>;
  on_open_change: (open: boolean) => void;
  open: boolean;
  trigger_class_name: string;
}>;

export function ModelSettingsPopover(props: ModelSettingsPopoverProps) {
  const categories: SettingsCascadeCategory[] = [];
  if (props.effort_options.length > 0) {
    const options: SettingsCascadeOption[] = [
      { value: "default", label: "默认", description: "使用当前模型的默认思考强度" },
      ...props.effort_options.map((option) => ({ value: option.key, label: option.label })),
    ];
    categories.push({
      id: "effort",
      separator_before: true,
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
    <ModelPicker
      title="模型设置" label={props.model_display_name}
      providers={props.providers} selection={props.selection} follow_default
      additional_categories={categories}
      disabled={props.disabled} disabled_reason={props.model_switch_disabled_reason}
      initial_category={props.initial_category === "effort" ? "effort" : null}
      onOpenChange={props.on_open_change} onSelect={props.on_model_change}
      open={props.open} trigger_class_name={props.trigger_class_name}
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
