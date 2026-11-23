import type {
  ScheduleExecutionReason, ScheduleExecutionStatus, ScheduleRule, ScheduleSnapshot, ScheduleWeekday,
} from "@ez-assistant/protocol";

export const weekdays: readonly [ScheduleWeekday, string][] = [
  ["monday", "周一"], ["tuesday", "周二"], ["wednesday", "周三"], ["thursday", "周四"],
  ["friday", "周五"], ["saturday", "周六"], ["sunday", "周日"],
];

export const schedule_frequencies = { once: "一次", daily: "每天", weekly: "每周" } as const;

export function ruleLabel(rule: ScheduleRule): string {
  switch (rule.type) {
    case "once": return `${rule.date} ${rule.time} · ${schedule_frequencies.once}`;
    case "daily": return `${schedule_frequencies.daily} ${rule.time}`;
    case "weekly": return `${weekdays.filter(([day]) => rule.weekdays.includes(day)).map(([, label]) => label).join("、")} ${rule.time}`;
  }
}

/** 时间文本已经由执行设备按自身时区生成。 */
export function deviceTime(value: string): string {
  return value.slice(0, 16).replace("T", " ");
}

export function planStatus(plan: ScheduleSnapshot): string {
  if (plan.deleting) return "正在删除";
  if (!plan.settings.enabled) return "已暂停";
  if (!plan.next_run_at && plan.settings.rule.type === "once") return plan.once_triggered ? "已触发" : "已错过";
  return "已启用";
}

export function scheduledTime(plan: ScheduleSnapshot): string {
  return plan.next_run_at ? deviceTime(plan.next_run_at) : "—";
}

export const executionStatus: Record<ScheduleExecutionStatus, string> = {
  running: "执行中", succeeded: "已完成", failed: "执行失败", timed_out: "已超时",
  cancelled: "已停止", skipped: "已跳过", outcome_unknown: "结果未知", cleanup_unconfirmed: "进程停止待确认",
};

export const executionReason: Record<ScheduleExecutionReason, string> = {
  previous_execution_active: "上一次执行仍占用执行名额。",
  workspace_unavailable: "执行所选的项目不可用。",
  shell_unavailable: "执行设备上的 Shell 不可用。",
  signalled: "脚本进程被信号终止。",
  host_interrupted: "Host 中断，本次结果无法确认。",
  cleanup_unconfirmed: "Host 尚未确认进程已停止。",
  cancelled: "本次执行已取消。",
  agent_preparation_failed: "智能体、模型或工作环境准备失败。",
  agent_run_failed: "智能体执行失败。",
  agent_approval_required: "所需工具授权无法在无人值守执行中获得。",
  background_failed: "关联后台进程执行失败。",
};

export function errorText(error: unknown): string {
  return error instanceof Error ? error.message : "操作失败。";
}
