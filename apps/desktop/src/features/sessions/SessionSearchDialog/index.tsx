import type { SessionSummary } from "@ez-assistant/protocol";
import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import { Button } from "../../../components/Button";
import { Dialog } from "../../../components/Dialog";
import { Icon } from "../../../components/Icon";
import { InlineIconButton } from "../../../components/InlineIconButton";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

type SearchScope = "active" | "archived";

/** 独立搜索弹窗只持有本次查询状态；会话及分页仍以 Runtime 投影为准。 */
export const SessionSearchDialog = observer(function SessionSearchDialog({ open, on_close }: Readonly<{
  open: boolean;
  on_close: () => void;
}>) {
  const store = useRootStore();
  const input_ref = useRef<HTMLInputElement>(null);
  const request_generation = useRef(0);
  const [scope, setScope] = useState<SearchScope>("active");
  const [query, setQuery] = useState("");
  const [matches, setMatches] = useState<SessionSummary[]>([]);
  const [search_next, setSearchNext] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const application = store.projection.application;
  const trimmed_query = query.trim();
  const sessions = trimmed_query
    ? matches
    : scope === "active" ? application?.active_sessions ?? [] : application?.archived_sessions ?? [];
  const active_workspaces = application?.workspaces.filter((workspace) => workspace.lifecycle === "active") ?? [];
  const active_workspace_ids = new Set(active_workspaces.map((workspace) => workspace.workspace_id));
  const visible_sessions = sessions.filter((session) => session.role !== "controller"
    && (!session.workspace_id || active_workspace_ids.has(session.workspace_id)));
  const next_offset = trimmed_query ? search_next : scope === "active"
    ? application?.active_sessions_next_offset ?? null : application?.archived_sessions_next_offset ?? null;

  useEffect(() => {
    if (!open) return;
    setScope("active");
    setQuery("");
    setMatches([]);
    setSearchNext(null);
    setLoading(false);
    setError(null);
  }, [open]);

  useEffect(() => {
    const generation = ++request_generation.current;
    setMatches([]);
    setSearchNext(null);
    setError(null);
    setLoading(false);
    if (!open || !trimmed_query) return;

    const timer = window.setTimeout(() => {
      setLoading(true);
      void store.listSessionPage(scope, 0, trimmed_query).then((page) => {
        if (request_generation.current !== generation) return;
        setMatches(page.sessions);
        setSearchNext(page.has_more ? 100 : null);
      }).catch((failure: unknown) => {
        if (request_generation.current === generation) {
          setError(failure instanceof Error ? failure.message : "搜索失败");
        }
      }).finally(() => {
        if (request_generation.current === generation) setLoading(false);
      });
    }, 180);
    return () => window.clearTimeout(timer);
  }, [open, scope, store, trimmed_query]);

  async function loadMore() {
    if (loading || next_offset == null) return;
    const generation = request_generation.current;
    setLoading(true);
    setError(null);
    try {
      if (trimmed_query) {
        const page = await store.listSessionPage(scope, next_offset, trimmed_query);
        if (request_generation.current !== generation) return;
        setMatches((current) => [...new Map([...current, ...page.sessions].map((session) => [session.session_id, session])).values()]);
        setSearchNext(page.has_more ? next_offset + 100 : null);
      } else {
        await store.loadMoreSessions(scope);
      }
    } catch (failure: unknown) {
      if (request_generation.current === generation) {
        setError(failure instanceof Error ? failure.message : "加载失败");
      }
    } finally {
      if (request_generation.current === generation) setLoading(false);
    }
  }

  function openSession(session: SessionSummary) {
    store.navigation.setListMode(scope);
    void store.selectSession(session.session_id);
    on_close();
  }

  return <Dialog
    aria_labelledby="session-search-dialog-title"
    backdrop_class_name={styles.backdrop}
    dialog_class_name={styles.dialog}
    initial_focus_ref={input_ref}
    on_close={on_close}
    open={open}
  >
    <header className={styles.header}>
      <h2 id="session-search-dialog-title">搜索会话</h2>
      <InlineIconButton className={styles.close_button} icon="x" label="关闭搜索" onClick={on_close} />
    </header>
    <div className={styles.search_controls}>
      <label className={styles.search_field}>
        <Icon name="search" size={17} />
        <input
          aria-label="搜索会话名称"
          onChange={(event) => setQuery(event.currentTarget.value)}
          placeholder="搜索会话名称"
          ref={input_ref}
          role="searchbox"
          type="text"
          value={query}
        />
      </label>
      <div aria-label="会话范围" className={styles.filter_row} role="group">
        <button aria-pressed={scope === "active"} onClick={() => setScope("active")} type="button">活动会话</button>
        <button aria-pressed={scope === "archived"} onClick={() => setScope("archived")} type="button">已归档</button>
      </div>
    </div>
    <div aria-label="会话搜索结果" className={styles.results} role="region">
      <p className={styles.summary}>{scope === "archived" ? "已归档" : "活动会话"} · {visible_sessions.length} 个结果</p>
      {visible_sessions.map((session) => {
        const workspace = active_workspaces.find((item) => item.workspace_id === session.workspace_id);
        return <button className={styles.result_row} key={session.session_id} onClick={() => openSession(session)} type="button">
          <span>{session.title}</span>
          <small>{workspace?.label ?? "无项目"}</small>
        </button>;
      })}
      {!loading && visible_sessions.length === 0 && <p className={styles.empty}>没有匹配的会话名称</p>}
      {loading && <p className={styles.loading} role="status">正在加载…</p>}
      {error && <p className={styles.error} role="alert">{error}</p>}
      {next_offset != null && <Button disabled={loading} onClick={() => void loadMore()} size="small" variant="text">加载更多</Button>}
    </div>
  </Dialog>;
});
