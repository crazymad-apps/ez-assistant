export const SLASH_COMMANDS = [
  { name: "/skill", description: "为下一条输入选择一个技能", picker: "skill" as const },
  { name: "/goal", description: "将下一次完整输入标记为自动续跑目标", picker: null },
  { name: "/model", description: "切换本会话模型", picker: "model" as const },
  { name: "/mode", description: "切换构建或规划模式", picker: "variant" as const },
  { name: "/approval", description: "切换询问或自动审批", picker: "approval" as const },
  { name: "/compact", description: "压缩较早上下文", picker: null },
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
