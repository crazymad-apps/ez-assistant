import type { ApplicationSnapshot, SessionGroupSnapshot, SessionSummary } from "@ez-assistant/protocol";

/** 将测试场景中的会话按真实项目归属组成协议页，不向产品保留全局列表字段。 */
export function sidebarFixture(input: Readonly<{
  active?: readonly SessionSummary[];
  archived?: readonly SessionSummary[];
  active_next?: number | null;
  archived_next?: number | null;
}> = {}): Pick<ApplicationSnapshot, "session_groups" | "pinned_sessions" | "workspaces_next_offset"> {
  const session_groups: SessionGroupSnapshot[] = [];
  for (const filter of ["active", "archived"] as const) {
    const rows = input[filter] ?? [];
    const workspaces = new Set(rows.map(row => row.workspace_id ?? null));
    workspaces.add(null);
    for (const workspace_id of workspaces) session_groups.push({
      group: workspace_id === null ? { type: "unbound" } : { type: "workspace", workspace_id },
      filter,
      sessions: rows.filter(row => (row.workspace_id ?? null) === workspace_id && row.role !== "controller"),
      next_offset: workspace_id === null ? input[`${filter}_next`] ?? null : null,
    });
  }
  return { session_groups, pinned_sessions: (input.active ?? []).filter(row => row.is_pinned && row.role !== "controller"), workspaces_next_offset: null };
}

export function sessionsIn(application: ApplicationSnapshot, filter: "active" | "archived" = "active"): SessionSummary[] {
  return application.session_groups.filter(page => page.filter === filter).flatMap(page => page.sessions);
}
