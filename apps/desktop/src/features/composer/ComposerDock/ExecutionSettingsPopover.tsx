import type { AgentVariant, ApprovalMode } from "../../../generated/assistant-protocol";
import {
  SettingsCascadePopover,
  type SettingsCascadeCategory,
} from "./SettingsCascadePopover";

type ExecutionSettingsPopoverProps = Readonly<{
  approval_mode: ApprovalMode;
  disabled: boolean;
  initial_category: "variant" | "approval" | null;
  on_approval_change: (mode: ApprovalMode) => Promise<boolean>;
  on_open_change: (open: boolean) => void;
  on_variant_change: (variant: AgentVariant) => Promise<boolean>;
  open: boolean;
  trigger_class_name: string;
  variant: AgentVariant;
}>;

export function ExecutionSettingsPopover(props: ExecutionSettingsPopoverProps) {
  const categories: readonly SettingsCascadeCategory[] = [
    {
      id: "variant",
      label: "执行模式",
      selected: props.variant,
      value_label: variantLabel(props.variant),
      options: [
        { value: "build", label: "Build", description: "允许在授权范围内修改与执行" },
        { value: "plan", label: "Plan", description: "仅分析与规划，不修改工作区" },
      ],
      on_select: (value) => props.on_variant_change(value as AgentVariant),
    },
    {
      id: "approval",
      label: "审批模式",
      selected: props.approval_mode,
      value_label: approvalLabel(props.approval_mode),
      options: [
        { value: "ask", label: "Ask", description: "敏感操作到达时请求确认" },
        { value: "auto", label: "Auto", description: "按已配置权限规则自动判断" },
      ],
      on_select: (value) => props.on_approval_change(value as ApprovalMode),
    },
  ];
  return (
    <SettingsCascadePopover
      aria_label="执行设置"
      categories={categories}
      disabled={props.disabled}
      initial_category={props.initial_category}
      on_open_change={props.on_open_change}
      open={props.open}
      trigger_class_name={props.trigger_class_name}
      trigger_content={`${variantLabel(props.variant)} · ${approvalLabel(props.approval_mode)}`}
    />
  );
}

function variantLabel(value: AgentVariant): string {
  return value === "build" ? "Build" : "Plan";
}

function approvalLabel(value: ApprovalMode): string {
  return value === "ask" ? "Ask" : "Auto";
}
