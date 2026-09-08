export function connectionLabel(state: string): string {
  return ({
    connected: "已连接",
    reconnecting: "重连中",
    disconnected: "已断开",
    component_mismatch: "组件不兼容",
    stopping_runtime: "停止中",
    restarting_runtime: "重启中",
    runtime_stopped: "已停止",
  } as Record<string, string>)[state] ?? "连接中";
}

export function configurationLabel(state?: string): string {
  return ({ ready: "可用", degraded: "部分可用", invalid: "无效", missing: "未配置" } as Record<string, string>)[state ?? ""] ?? "—";
}

export function formatDateTime(value: number | null): string {
  if (value === null) return "—";
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(new Date(value));
}
