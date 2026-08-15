import type { AgentVariant, ApprovalMode } from "../../../generated/assistant-protocol";
import type { SelectionOption } from "../../../components/SelectionPopover";

export type PickerName = "model" | "variant" | "approval" | null;

export const VARIANT_OPTIONS: readonly SelectionOption<AgentVariant>[] = [
  { value: "build", label: "Build 模式", description: "允许在授权范围内修改与执行" },
  { value: "plan", label: "Plan 模式", description: "仅分析与规划，不修改工作区" },
];

export const APPROVAL_OPTIONS: readonly SelectionOption<ApprovalMode>[] = [
  { value: "ask", label: "Ask 权限", description: "敏感操作到达时请求确认" },
  { value: "auto", label: "Auto 权限", description: "按已配置规则自动判断" },
];

export const SLASH_COMMANDS = [
  { name: "/model", description: "切换本会话模型", picker: "model" as const },
  { name: "/mode", description: "切换 Build 或 Plan", picker: "variant" as const },
  { name: "/approval", description: "切换 Ask 或 Auto", picker: "approval" as const },
  { name: "/new", description: "新建会话", picker: null },
  { name: "/help", description: "查看指令与键盘说明", picker: null },
] as const;

export type SlashCommand = typeof SLASH_COMMANDS[number];

export function formatCompact(value: number): string {
  return value >= 1_000
    ? `${(value / 1_000).toFixed(value >= 10_000 ? 0 : 1)}K`
    : String(value);
}
