import type { SessionSummary } from "@ez-assistant/protocol";
import { Button } from "../../../components/Button";
import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../../components/DropdownMenu";
import { Icon } from "../../../components/Icon";
import { Collapse } from "../../../components/Collapse";
import { Tooltip } from "../../../components/Tooltip";
import { useRootStore } from "../../../stores/RootStoreContext";
import { draftKeyForWorkspace } from "../../../stores/NewSessionDraftStore";
import { workspaceDisplayName } from "../sessionFormatters";
import { SessionList, WorkspaceGroup } from "./WorkspaceGroup";
import { ControllerSection } from "./ControllerSection";
import styles from "./index.module.scss";

export const SessionSidebar = observer(function SessionSidebar() {
  const store = useRootStore();
  const application = store.projection.application;
  const [search_open, setSearchOpen] = useState(false);
  const [workspace_section_open, setWorkspaceSectionOpen] = useState(true);
  const [unbound_section_open, setUnboundSectionOpen] = useState(true);
  const search_generation = useRef(0);
  const [search_sessions, setSearchSessions] = useState<SessionSummary[]>([]);
  const [search_next, setSearchNext] = useState<number | null>(null);
  const [list_loading, setListLoading] = useState(false);
  const [list_error, setListError] = useState<string | null>(null);
  const search_input_ref = useRef<HTMLInputElement>(null);
  const sessions =
    store.navigation.list_mode === "active"
      ? (application?.active_sessions ?? [])
      : (application?.archived_sessions ?? []);
  const query = store.navigation.search_query.trim();
  const workspaces = application?.workspaces ?? [];
  const active_workspaces = workspaces.filter((workspace) => workspace.lifecycle === "active");
  const active_workspace_ids = new Set(active_workspaces.map((workspace) => workspace.workspace_id));
  const visible_sessions = sessions.filter((session) => (
    session.role !== "controller"
    && (!session.workspace_id || active_workspace_ids.has(session.workspace_id))
  ));
  const filtered_sessions = search_sessions.filter((session) => session.role !== "controller" && (!session.workspace_id || active_workspace_ids.has(session.workspace_id)));
  const next_offset = search_open ? search_next : store.navigation.list_mode === "active" ? application?.active_sessions_next_offset : application?.archived_sessions_next_offset;

  useEffect(() => {
    search_generation.current += 1;
    let cancelled = false;
    setSearchSessions([]);
    setSearchNext(null);
    setListError(null);
    if (!search_open || !query) return;
    const timer = window.setTimeout(() => {
      setListLoading(true);
      void store.listSessionPage(store.navigation.list_mode, 0, query).then((page) => {
        if (cancelled) return;
        setSearchSessions(page.sessions);
        setSearchNext(page.has_more ? 100 : null);
      }).catch((error: unknown) => {
        if (!cancelled) setListError(error instanceof Error ? error.message : "加载失败");
      }).finally(() => { if (!cancelled) setListLoading(false); });
    }, 180);
    return () => { cancelled = true; window.clearTimeout(timer); setListLoading(false); };
  }, [store, search_open, query, store.navigation.list_mode]);

  async function loadMore() {
    if (list_loading || next_offset == null) return;
    setListLoading(true);
    setListError(null);
    const generation = search_generation.current;
    try {
      if (search_open) {
        const page = await store.listSessionPage(store.navigation.list_mode, next_offset, query);
        if (generation !== search_generation.current) return;
        setSearchSessions((current) => [...new Map([...current, ...page.sessions].map((session) => [session.session_id, session])).values()]);
        setSearchNext(page.has_more ? next_offset + 100 : null);
      } else {
        await store.loadMoreSessions(store.navigation.list_mode);
      }
    } catch (error) {
      if (generation !== search_generation.current) return;
      setListError(error instanceof Error ? error.message : "加载失败");
    } finally { if (generation === search_generation.current) setListLoading(false); }
  }
  const groups = active_workspaces
    .map((workspace) => ({
      workspace,
      sessions: visible_sessions.filter((session) => session.workspace_id === workspace.workspace_id),
    }));
  const unbound = visible_sessions.filter((session) => !session.workspace_id);

  useEffect(() => {
    if (search_open) {
      search_input_ref.current?.focus();
    }
  }, [search_open]);

  useEffect(() => {
    if (!search_open) {
      return undefined;
    }

    function handleEscape(event: KeyboardEvent) {
      if (event.key === "Escape") {
        closeSearch();
      }
    }

    document.addEventListener("keydown", handleEscape);
    return () => {
      document.removeEventListener("keydown", handleEscape);
    };
  }, [search_open]);

  function closeSearch() {
    setSearchOpen(false);
    store.navigation.setSearchQuery("");
  }

  return (
    <aside
      aria-label="会话导航"
      className={styles.sidebar}
      data-search-open={search_open}
    >
      <div className={styles.primary_actions}>
        {search_open ? (
          <label className={styles.search_field}>
            <Icon name="search" size={15} />
            <input
              aria-label="搜索会话名称"
              onChange={(event) => store.navigation.setSearchQuery(event.currentTarget.value)}
              placeholder="搜索会话名称"
              ref={search_input_ref}
              role="searchbox"
              type="text"
              value={store.navigation.search_query}
            />
            <button aria-label="关闭搜索" onClick={closeSearch} type="button">
              <Icon name="x" size={14} />
            </button>
          </label>
        ) : (
          <div className={styles.new_session_row}>
            <DropdownMenu className={styles.new_session_menu_root}>
              <DropdownMenuTrigger
                className={styles.new_session}
                disabled={
                  store.connection.state !== "connected" ||
                  store.pending_session_action ||
                  store.pending_workspace_action
                }
              >
                <Icon name="plus" size={17} />
                新对话
                <Icon className={styles.new_session_chevron} name="chevron-down" size={13} />
              </DropdownMenuTrigger>
              <DropdownMenuContent
                align="start"
                aria-label="选择新会话目录"
                className={styles.new_session_menu}
              >
                {active_workspaces.map((workspace) => (
                  <DropdownMenuItem
                    key={workspace.workspace_id}
                    onSelect={() => store.openNewSessionDraft(workspace.workspace_id)}
                  >
                    <Icon name="folder" size={15} />
                    <span>{workspace.label}</span>
                    {store.new_session_drafts.hasDraft(draftKeyForWorkspace(workspace.workspace_id)) && (
                      <em className={styles.draft_indicator}>有草稿</em>
                    )}
                    {workspaceDisplayName(workspace.user_directory) !== workspace.label && (
                      <small>{workspaceDisplayName(workspace.user_directory)}</small>
                    )}
                  </DropdownMenuItem>
                ))}
                <DropdownMenuItem onSelect={() => store.openNewSessionDraft(null)}>
                  <Icon name="message" size={15} />
                  <span>未绑定工作空间</span>
                  {store.new_session_drafts.hasDraft(draftKeyForWorkspace(null)) && (
                    <em className={styles.draft_indicator}>有草稿</em>
                  )}
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => void store.addWorkspace()}>
                  <Icon name="plus" size={15} />
                  <span>选择其他目录…</span>
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
            <button
              aria-label="搜索会话"
              className={styles.search_trigger}
              onClick={() => setSearchOpen(true)}
              type="button"
            >
              <Icon name="search" size={18} />
            </button>
          </div>
        )}
      </div>

      {!search_open && (
        <div className={styles.list_tabs} role="tablist" aria-label="会话范围">
          <button
            aria-selected={store.navigation.list_mode === "active"}
            onClick={() => store.navigation.setListMode("active")}
            role="tab"
            type="button"
          >
            会话
          </button>
          <button
            aria-selected={store.navigation.list_mode === "archived"}
            onClick={() => store.navigation.setListMode("archived")}
            role="tab"
            type="button"
          >
            已归档
          </button>
        </div>
      )}

      <div className={styles.session_scroll}>
        {store.navigation.list_mode === "active" && (
          <ControllerSection application={application} />
        )}
        {search_open ? (
          <section aria-label="会话搜索结果" className={styles.search_results}>
            <div className={styles.search_summary}>
              {!query ? "输入会话名称进行搜索" : `${filtered_sessions.length} 个结果`}
            </div>
            {filtered_sessions.length > 0 && (
              <SessionList indent="root" sessions={filtered_sessions} />
            )}
            {query && filtered_sessions.length === 0 && (
              <p className={styles.search_empty}>没有匹配的会话名称</p>
            )}
          </section>
        ) : (
          <>
            <div className={styles.section_header}>
              <button
                aria-expanded={workspace_section_open}
                className={styles.section_label}
                onClick={() => setWorkspaceSectionOpen((open) => !open)}
                type="button"
              >
                工作空间
              </button>
              <Tooltip content="添加工作空间">
                <button
                  aria-label="添加工作空间"
                  className={styles.section_action}
                  disabled={
                    store.connection.state !== "connected" ||
                    store.pending_workspace_action ||
                    store.pending_session_action
                  }
                  onClick={() => void store.addWorkspace()}
                  type="button"
                >
                  <Icon name="plus" size={14} />
                </button>
              </Tooltip>
              <button
                aria-expanded={workspace_section_open}
                aria-label={workspace_section_open ? "收起工作空间" : "展开工作空间"}
                className={styles.section_chevron}
                onClick={() => setWorkspaceSectionOpen((open) => !open)}
                type="button"
              >
                <Icon name="chevron-down" size={14} />
              </button>
            </div>
            <Collapse open={workspace_section_open}>
              <div>
                {groups.map((group) => (
                  <WorkspaceGroup
                    key={group.workspace.workspace_id}
                    sessions={group.sessions}
                    workspace={group.workspace}
                  />
                ))}
              </div>
            </Collapse>
            {unbound.length > 0 && (
              <section className={styles.unbound_section}>
                <div className={styles.section_header}>
                  <button
                    aria-expanded={unbound_section_open}
                    className={styles.section_label}
                    onClick={() => setUnboundSectionOpen((open) => !open)}
                    type="button"
                  >
                    独立会话
                  </button>
                  {store.navigation.list_mode === "active" && (
                    <Tooltip content="新建独立会话">
                      <button
                        aria-label="新建独立会话"
                        className={styles.section_action}
                        disabled={
                          store.connection.state !== "connected" ||
                          store.pending_session_action ||
                          store.pending_workspace_action
                        }
                        onClick={() => store.openNewSessionDraft(null)}
                        type="button"
                      >
                        <Icon name="plus" size={14} />
                      </button>
                    </Tooltip>
                  )}
                  <button
                    aria-expanded={unbound_section_open}
                    aria-label={unbound_section_open ? "收起独立会话" : "展开独立会话"}
                    className={styles.section_chevron}
                    onClick={() => setUnboundSectionOpen((open) => !open)}
                    type="button"
                  >
                    <Icon name="chevron-down" size={14} />
                  </button>
                </div>
                <Collapse open={unbound_section_open}>
                  <SessionList indent="root" sessions={unbound} />
                </Collapse>
              </section>
            )}
            {sessions.length === 0 && <div className={styles.empty_list}>暂无会话</div>}
          </>
        )}
        {next_offset != null && <Button variant="text" size="small" disabled={list_loading} onClick={() => void loadMore()}>{list_loading ? "加载中…" : "加载更多"}</Button>}
        {list_error && <p role="alert">{list_error}</p>}
      </div>
      <button className={styles.settings_button} onClick={() => store.settings.open()} type="button">
        <Icon name="settings" size={16} />
        <span>设置</span>
        <Icon name="chevron-right" size={14} />
      </button>
    </aside>
  );
});
