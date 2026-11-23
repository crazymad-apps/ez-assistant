import { useEffect, useMemo, useRef, useState } from "react";
import type { ContextPage, ContextPageRequest, RuntimeEvent, SessionId } from "@ez-assistant/protocol";
import type { RuntimeClient } from "../../../runtime-client/RuntimeClient";
import { useRootStore } from "../../../stores/RootStoreContext";

const PAGE_SIZE = 10;
export type ContextList = "runs" | "children" | "attachments" | "artifacts" | "skills";

type Result<T> = Readonly<{ rows: readonly T[]; page: ContextPage; diagnostic_count?: number }>;
type Query<T> = (client: RuntimeClient, session_id: SessionId, page: ContextPageRequest, signal: AbortSignal) => Promise<Result<T>>;

export function useContextPage<T>(
  kind: ContextList, client: RuntimeClient | null, session_id: SessionId | null,
  generation: number | null, enabled: boolean, query: Query<T>,
) {
  const root = useRootStore();
  const scope = useMemo(() => ({ requested: 0, revision: 0 }), [client, session_id]);
  const [revision, setRevision] = useState(0);
  const [current, setCurrent] = useState<Readonly<{
    scope: typeof scope;
    result: Result<T> | null;
    loading: boolean;
    error: string | null;
  }> | null>(null);
  const query_ref = useRef(query);
  query_ref.current = query;
  const refresh = () => {
    scope.revision += 1;
    setRevision((value) => value + 1);
  };

  useEffect(() => {
    if (!client || !session_id || !enabled) return;
    // 合并同一交换的可靠提交和状态事件。任何新读都取消上一份结果的接纳权。
    let timer: number | undefined;
    const invalidate = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        scope.revision += 1;
        setRevision((value) => value + 1);
      }, 80);
    };
    const unsubscribe = root.subscribeRuntimeEvents(client, (event) => {
      if (affectsPage(kind, event, session_id)) invalidate();
    });
    window.addEventListener("focus", invalidate);
    return () => { unsubscribe(); window.clearTimeout(timer); window.removeEventListener("focus", invalidate); };
  }, [root, client, session_id, enabled, kind, scope]);

  useEffect(() => {
    if (!enabled || !client || !session_id) return;
    const controller = new AbortController();
    const { signal } = controller;
    const requested_revision = scope.revision;
    setCurrent((previous) => ({ scope, result: previous?.scope === scope ? previous.result : null, loading: true, error: null }));
    void (async () => {
      let offset = scope.requested * PAGE_SIZE;
      for (;;) {
        const result = await query_ref.current(client, session_id, { offset, limit: PAGE_SIZE }, signal);
        if (signal.aborted || scope.revision !== requested_revision) return;
        const last_offset = Math.max(0, Math.ceil(result.page.total / PAGE_SIZE) - 1) * PAGE_SIZE;
        if (offset > last_offset && result.page.total > 0) {
          offset = last_offset;
          continue;
        }
        scope.requested = Math.min(offset, last_offset) / PAGE_SIZE;
        setCurrent({ scope, result: { ...result, page: { ...result.page, offset: scope.requested * PAGE_SIZE } }, loading: false, error: null });
        return;
      }
    })().catch((error: unknown) => {
      if (!signal.aborted && scope.revision === requested_revision) setCurrent((previous) => ({ scope, result: previous?.scope === scope ? previous.result : null, loading: false,
        error: error instanceof Error ? error.message : "无法读取列表。" }));
    });
    return () => controller.abort();
  }, [client, session_id, scope, generation, enabled, revision]);

  const visible = current?.scope === scope ? current : null;
  const page = visible?.result?.page;
  return {
    rows: visible?.result?.rows ?? [], total: page?.total ?? 0, diagnostics: visible?.result?.diagnostic_count ?? 0,
    page: Math.floor((page?.offset ?? 0) / PAGE_SIZE), pages: Math.max(1, Math.ceil((page?.total ?? 0) / PAGE_SIZE)),
    loading: enabled && (visible?.loading ?? true), error: visible?.error ?? null, refresh,
    navigate(next: number) { scope.requested = next; refresh(); },
  };
}

function affectsPage(kind: ContextList, event: RuntimeEvent, session_id: SessionId): boolean {
  if (kind === "skills" && (event.type === "plugin_catalog_changed" || event.type === "skill_settings_changed" || event.type === "workspace_changed")) return true;
  switch (event.type) {
    case "conversation_committed":
      return event.owner.type === "main_session" && event.owner.session_id === session_id
        && (kind === "artifacts" || kind === "skills" || kind === "children");
    case "session_changed": return event.session_id === session_id && kind === "attachments";
    case "run_accepted":
    case "run_started":
    case "run_cancelling":
    case "run_finished": return event.session_id === session_id && kind === "runs";
    case "child_task_event":
      return event.session_id === session_id && kind === "children" && ["created", "started", "finished"].includes(event.event.type);
    case "approval_requested": return kind === "children" && event.approval.session_id === session_id && Boolean(event.approval.child_task_id);
    case "approval_resolved": return kind === "children" && event.session_id === session_id && Boolean(event.child_task_id);
    default: return false;
  }
}
