import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../../components/DropdownMenu";
import { Icon } from "../../../components/Icon";
import { Tooltip } from "../../../components/Tooltip";
import type { SessionSummary } from "../../../generated/assistant-protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { sessionTime, workspaceDisplayName } from "../sessionFormatters";
import { SessionList, WorkspaceGroup } from "./WorkspaceGroup";
import styles from "./index.module.scss";

export const SessionSidebar = observer(function SessionSidebar() {
  const store = useRootStore();
  const application = store.projection.application;
  const [search_open, setSearchOpen] = useState(false);
  const [workspace_section_open, setWorkspaceSectionOpen] = useState(true);
  const [unbound_section_open, setUnboundSectionOpen] = useState(true);
  const search_input_ref = useRef<HTMLInputElement>(null);
  const sessions =
    store.navigation.list_mode === "active"
      ? (application?.active_sessions ?? [])
      : (application?.archived_sessions ?? []);
  const all_sessions = [
    ...(application?.active_sessions ?? []),
    ...(application?.archived_sessions ?? []),
  ];
  const query = store.navigation.search_query.trim().toLocaleLowerCase();
  const search_results = all_sessions.filter((session) => {
    if (!query) {
      return true;
    }
    const workspace = application?.workspaces.find(
      (item) => item.workspace_id === session.workspace_id,
    );
    return (
      session.title.toLocaleLowerCase().includes(query) ||
      workspace?.user_directory.toLocaleLowerCase().includes(query)
    );
  });
  const groups = (application?.workspaces ?? []).map((workspace) => ({
    workspace,
    sessions: sessions.filter((session) => session.workspace_id === workspace.workspace_id),
  }));
  const unbound = sessions.filter((session) => !session.workspace_id);

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

  function selectSearchResult(session: SessionSummary) {
    const is_archived = application?.archived_sessions.some(
      (item) => item.session_id === session.session_id,
    );
    store.navigation.setListMode(is_archived ? "archived" : "active");
    closeSearch();
    void store.selectSession(session.session_id);
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
              aria-label="搜索会话"
              onChange={(event) => store.navigation.setSearchQuery(event.currentTarget.value)}
              placeholder="搜索会话"
              ref={search_input_ref}
              type="search"
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
                {(application?.workspaces ?? []).map((workspace) => (
                  <DropdownMenuItem
                    key={workspace.workspace_id}
                    onSelect={() => void store.createSession(workspace.workspace_id)}
                  >
                    <Icon name="folder" size={15} />
                    <span>{workspaceDisplayName(workspace.user_directory)}</span>
                  </DropdownMenuItem>
                ))}
                <DropdownMenuItem onSelect={() => void store.createSession(null)}>
                  <Icon name="message" size={15} />
                  <span>独立会话</span>
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
        {search_open ? (
          <section aria-label="会话搜索结果" className={styles.search_results}>
            <div className={styles.search_summary}>
              {query ? `${search_results.length} 个结果` : "全部会话"}
            </div>
            {search_results.map((session) => {
              const workspace = application?.workspaces.find(
                (item) => item.workspace_id === session.workspace_id,
              );
              const is_archived = application?.archived_sessions.some(
                (item) => item.session_id === session.session_id,
              );
              return (
                <button
                  key={session.session_id}
                  onClick={() => selectSearchResult(session)}
                  type="button"
                >
                  <Icon name="message" size={14} />
                  <span className={styles.search_result_title}>{session.title}</span>
                  <small>{workspace ? workspaceDisplayName(workspace.user_directory) : "独立会话"}</small>
                  <span className={styles.search_result_meta}>
                    {is_archived ? "已归档" : sessionTime(session)}
                  </span>
                </button>
              );
            })}
            {search_results.length === 0 && <p className={styles.search_empty}>没有匹配的会话</p>}
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
            {workspace_section_open && (
              groups.map((group) => (
                <WorkspaceGroup
                  key={group.workspace.workspace_id}
                  sessions={group.sessions}
                  workspace={group.workspace}
                />
              ))
            )}
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
                        onClick={() => void store.createSession(null)}
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
                {unbound_section_open && (
                  <SessionList indent="root" sessions={unbound} />
                )}
              </section>
            )}
            {sessions.length === 0 && <div className={styles.empty_list}>暂无会话</div>}
          </>
        )}
      </div>
      <button className={styles.settings_button} onClick={() => store.settings.open()} type="button">
        <Icon name="settings" size={16} />
        <span>设置</span>
        <Icon name="chevron-right" size={14} />
      </button>
    </aside>
  );
});
