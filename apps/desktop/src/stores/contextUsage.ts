import type {
  ContextUsageSnapshot,
  RunId,
  TokenUsageSnapshot,
} from "@ez-assistant/protocol";

/**
 * 将可靠会话快照与当前活动 Run 的最新 Provider 用量合并成展示口径。
 *
 * Runtime 快照仍是窗口大小和持久事实来源；活动 Run 只在身份吻合时，用最近一个
 * 已完成模型 step 的 total_tokens 推进旧占用，避免等待下一次会话快照刷新。可靠快照中可能已经
 * 包含刚写入的 Tool Result 增量，因此同一 generation 内不允许 live 值把它回退。
 */
export function effectiveContextUsage(
  context: ContextUsageSnapshot | null,
  active_run_id: RunId | null,
  live_run_id: string | null,
  live_usage: TokenUsageSnapshot | null,
): ContextUsageSnapshot | null {
  if (!context || !active_run_id || active_run_id !== live_run_id || !live_usage) {
    return context;
  }
  const used_tokens = Math.max(context.used_tokens, live_usage.total_tokens);
  const usage_basis_points = Math.min(
    10_000,
    Math.floor((used_tokens * 10_000) / Math.max(1, context.window_tokens)),
  );
  return {
    used_tokens,
    window_tokens: context.window_tokens,
    usage_basis_points,
  };
}
