import type { SessionSummary } from "../../generated/assistant-protocol";

export function workspaceDisplayName(path: string): string {
  const normalized = path.replaceAll("\\", "/").replace(/\/$/, "");
  return normalized.split("/").at(-1) || path;
}

export function sessionTime(session: SessionSummary): string {
  if (session.active_run_id) {
    return "";
  }
  const timestamp = session.updated_at_ms ?? session.created_at_ms;
  if (!timestamp) {
    return "";
  }
  const date = new Date(timestamp);
  const now = new Date();
  if (date.toDateString() === now.toDateString()) {
    return new Intl.DateTimeFormat("zh-CN", {
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    }).format(date);
  }
  return new Intl.DateTimeFormat("zh-CN", { month: "numeric", day: "numeric" }).format(date);
}
