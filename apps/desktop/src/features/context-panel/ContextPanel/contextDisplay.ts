import type { RunStatus, SessionViewSnapshot } from "../../../generated/assistant-protocol";

/** 只组合会话冻结目录与激活投影；可用不等于已加载，不触发扫描或修改激活状态。 */
export function sessionSkillRows(view: SessionViewSnapshot | undefined) {
  const active = (view?.active_skills ?? []).map((skill) => ({
    name: skill.tag.name,
    status_label: skill.trigger === "user" ? "用户激活" : "智能体激活",
  }));
  const active_names = new Set(active.map((skill) => skill.name));
  const catalog = view?.skill_catalog;
  const available = catalog?.status === "ready" ? catalog.skills : [];
  return [
    ...active,
    ...available
      .filter((skill) => skill.enabled && skill.health === "ready" && !active_names.has(skill.name))
      .map((skill) => ({ name: skill.name, status_label: "可用" })),
  ];
}

export function formatNullableTokens(value: number | null): string {
  return value === null ? "未提供" : formatTokens(value);
}

export function formatBasisPoints(value: number | null): string {
  return value === null ? "未提供" : `${(value / 100).toFixed(1)}%`;
}

export function formatTokens(value: number): string {
  return value >= 1_000 ? `${(value / 1_000).toFixed(value >= 10_000 ? 0 : 1)}K` : value.toLocaleString("zh-CN");
}

export function formatBytes(value: number): string {
  if (value >= 1024 * 1024) {
    return `${(value / (1024 * 1024)).toFixed(1)} MB`;
  }
  if (value >= 1024) {
    return `${(value / 1024).toFixed(1)} KB`;
  }
  return `${value} B`;
}

export function formatVariant(variant: string | null | undefined): string {
  return variant === "plan" ? "规划" : variant === "build" ? "构建" : "未记录";
}

export function formatApprovalMode(mode: string | null | undefined): string {
  return mode === "ask" ? "询问" : mode === "auto" ? "自动" : "未记录";
}

export function formatModelIdentity(display_name: string | undefined, model_key: string | null | undefined): string {
  if (display_name) {
    return display_name;
  }
  return model_key ? `${model_key}（历史配置）` : "未记录";
}

export function runStatusLabel(status: RunStatus): string {
  return {
    accepted: "等待",
    running: "执行中",
    cancelling: "取消中",
    completed: "完成",
    failed: "失败",
    cancelled: "已取消",
    interrupted: "已中断",
    compaction_required: "需压缩",
  }[status];
}

export function sessionStatusLabel(
  lifecycle: string | null | undefined,
  active_status: RunStatus | null | undefined,
  approval_count: number,
  resume_required: boolean | null | undefined,
): string {
  if (lifecycle === "archived") {
    return "已归档";
  }
  if (approval_count > 0) {
    return "等待审批";
  }
  if (resume_required) {
    return "等待继续";
  }
  return active_status ? runStatusLabel(active_status) : "空闲";
}

export function formatRunTime(value: number | null | undefined): string {
  if (value === null || value === undefined) {
    return "时间未记录";
  }
  const date = new Date(value);
  const pad = (part: number) => String(part).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

export function childStatusLabel(
  status: "accepted" | "running" | "completed" | "failed" | "cancelled" | "interrupted",
): string {
  return {
    accepted: "等待",
    running: "执行中",
    completed: "完成",
    failed: "失败",
    cancelled: "取消",
    interrupted: "中断",
  }[status];
}
