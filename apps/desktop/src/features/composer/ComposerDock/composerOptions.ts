export const SLASH_COMMANDS = [
  { name: "/goal", description: "将下一次完整输入标记为自动续跑目标", picker: null },
  { name: "/model", description: "切换本会话模型", picker: "model" as const },
  { name: "/mode", description: "切换 Build 或 Plan", picker: "variant" as const },
  { name: "/approval", description: "切换 Ask 或 Auto", picker: "approval" as const },
  { name: "/new", description: "新建会话", picker: null },
  { name: "/help", description: "查看指令与键盘说明", picker: null },
] as const;

export type SlashCommand = typeof SLASH_COMMANDS[number];

export type SlashCommandItem = SlashCommand & Readonly<{
  disabled_reason: string | null;
}>;

export function formatCompact(value: number): string {
  return value >= 1_000
    ? `${(value / 1_000).toFixed(value >= 10_000 ? 0 : 1)}K`
    : String(value);
}
