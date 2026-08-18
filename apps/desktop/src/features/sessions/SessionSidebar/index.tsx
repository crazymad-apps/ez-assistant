import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../../components/DropdownMenu";
import { Icon } from "../../../components/Icon";
import { SelectionPopover, type SelectionOption } from "../../../components/SelectionPopover";
import { Tooltip } from "../../../components/Tooltip";
import type {
  ApplicationSnapshot,
  ConversationHistoryHit,
  ConversationHistoryScope,
  SessionId,
} from "../../../generated/assistant-protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { workspaceDisplayName } from "../sessionFormatters";
import { SessionList, WorkspaceGroup } from "./WorkspaceGroup";
import styles from "./index.module.scss";

export const SessionSidebar = observer(function SessionSidebar() {
  const store = useRootStore();
  const application = store.projection.application;
  const [search_open, setSearchOpen] = useState(false);
  const [workspace_section_open, setWorkspaceSectionOpen] = useState(true);
  const [unbound_section_open, setUnboundSectionOpen] = useState(true);
  const [scope_open, setScopeOpen] = useState(false);
  const search_input_ref = useRef<HTMLInputElement>(null);
  const sessions =
    store.navigation.list_mode === "active"
      ? (application?.active_sessions ?? [])
      : (application?.archived_sessions ?? []);
  const query = store.navigation.search_query.trim();
  const search_groups = groupConversationHistoryHits(store.conversation_search.items);
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
    if (!search_open || !query || !store.navigation.selected_session_id) {
      return undefined;
    }
    const timer = window.setTimeout(() => {
      void store.searchConversationHistory(true);
    }, 250);
    return () => window.clearTimeout(timer);
  }, [query, search_open, store, store.conversation_search.scope, store.navigation.selected_session_id]);

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
    setScopeOpen(false);
    store.navigation.setSearchQuery("");
    store.conversation_search.reset();
  }

  const scope_options: readonly SelectionOption<ConversationHistoryScope>[] = [
    { value: "session", label: "当前会话", description: "只检索当前主会话及其子任务" },
    ...(sessionWorkspaceId(application, store.navigation.selected_session_id)
      ? [{ value: "workspace" as const, label: "Workspace", description: "检索当前工作空间的全部会话" }]
      : []),
    { value: "global", label: "全局", description: "检索本机全部会话" },
  ];

  return (
    <aside
      aria-label="会话导航"
      className={styles.sidebar}
      data-search-open={search_open}
    >
      <div className={styles.primary_actions}>
        {search_open ? (
          <div className={styles.search_controls}>
            <label className={styles.search_field}>
              <Icon name="search" size={15} />
              <input
                aria-label="搜索历史会话"
                onChange={(event) => {
                  const next_query = event.currentTarget.value;
                  store.navigation.setSearchQuery(next_query);
                  store.conversation_search.setQuery(next_query);
                }}
                placeholder="搜索标题和消息正文"
                ref={search_input_ref}
                type="search"
                value={store.navigation.search_query}
              />
              <button aria-label="关闭搜索" onClick={closeSearch} type="button">
                <Icon name="x" size={14} />
              </button>
            </label>
            <SelectionPopover
              aria_label="选择历史检索范围"
              content_width="content"
              open={scope_open}
              on_open_change={setScopeOpen}
              on_select={(scope) => store.conversation_search.setScope(scope)}
              options={scope_options}
              selected={store.conversation_search.scope}
              trigger_class_name={styles.scope_trigger}
              trigger_variant="compact"
            />
          </div>
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
              {!query
                ? "输入关键词搜索历史会话"
                : store.conversation_search.status === "loading"
                  ? "正在搜索…"
                  : `${store.conversation_search.items.length} 个结果`}
            </div>
            {store.conversation_search.error && (
              <div className={styles.search_error} role="alert">
                <p>{store.conversation_search.error}</p>
                <button onClick={() => void store.searchConversationHistory(true)} type="button">
                  重试
                </button>
              </div>
            )}
            {store.conversation_search.partial && (
              <p className={styles.search_notice}>部分历史暂不可用，已显示其余检索结果。</p>
            )}
            {search_groups.map((group) => (
              <section className={styles.search_group} key={group.key}>
                <header className={styles.search_group_header}>
                  <Icon name="message" size={14} />
                  <strong>{group.title}</strong>
                  {group.parent_title && <small>{group.parent_title}</small>}
                </header>
                {group.hits.map((hit) => (
                  <button
                    className={styles.search_result}
                    key={`${hit.message_id ?? "title"}-${hit.match_kind}-${hit.created_at_ms ?? "none"}`}
                    onClick={() => void store.selectConversationHistoryHit(hit)}
                    type="button"
                  >
                    <span className={styles.search_result_heading}>
                      <span>{hit.match_kind === "title" ? "标题" : "消息"}</span>
                      <time>{formatSearchTime(hit.created_at_ms)}</time>
                    </span>
                    <span className={styles.search_result_snippet}>{hit.snippet}</span>
                  </button>
                ))}
              </section>
            ))}
            {query
              && store.conversation_search.status === "ready"
              && store.conversation_search.items.length === 0
              && <p className={styles.search_empty}>没有匹配的历史内容</p>}
            {store.conversation_search.next_offset !== null && (
              <button
                className={styles.load_more}
                disabled={store.conversation_search.status === "loading"}
                onClick={() => void store.searchConversationHistory(false)}
                type="button"
              >
                加载更多
              </button>
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

function sessionWorkspaceId(
  application: ApplicationSnapshot | null | undefined,
  session_id: SessionId | null,
): ApplicationSnapshot["workspaces"][number]["workspace_id"] | null {
  if (!application || !session_id) return null;
  const sessions = [...application.active_sessions, ...application.archived_sessions];
  return sessions.find((session) => session.session_id === session_id)?.workspace_id ?? null;
}

function formatSearchTime(timestamp: number | null): string {
  if (timestamp === null) return "";
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(timestamp);
}

export type ConversationHistoryHitGroup = Readonly<{
  key: string;
  title: string;
  parent_title: string | null;
  hits: readonly ConversationHistoryHit[];
}>;

/** 搜索结果按实际 Conversation owner 分组，主会话与各子任务保持独立来源。 */
export function groupConversationHistoryHits(
  hits: readonly ConversationHistoryHit[],
): readonly ConversationHistoryHitGroup[] {
  const groups = new Map<string, {
    title: string;
    parent_title: string | null;
    hits: ConversationHistoryHit[];
  }>();
  for (const hit of hits) {
    const key = hit.owner.type === "child_task"
      ? `child:${hit.owner.session_id}:${hit.owner.child_task_id}`
      : `session:${hit.owner.session_id}`;
    const existing = groups.get(key);
    if (existing) {
      existing.hits.push(hit);
      continue;
    }
    groups.set(key, {
      title: hit.child_task_title ?? hit.session_title,
      parent_title: hit.child_task_title ? hit.session_title : null,
      hits: [hit],
    });
  }
  return [...groups.entries()].map(([key, group]) => ({ key, ...group }));
}
