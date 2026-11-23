import { revealFileLabel } from "../../../platform/fileManager";
import { observer } from "mobx-react-lite";
import { useEffect, useMemo, useState, type KeyboardEvent, type MouseEvent } from "react";
import type {
  AttachmentSummary,
  SessionResourceLocator,
  SessionViewSnapshot,
  SystemContextSnapshot,
} from "@ez-assistant/protocol";
import { Button } from "../../../components/Button";
import { EmptyRow } from "../../../components/EmptyRow";
import { Icon } from "../../../components/Icon";
import { InlineIconButton } from "../../../components/InlineIconButton";
import { PresenceBoundary } from "../../../components/Presence";
import { Tooltip } from "../../../components/Tooltip";
import { useRootStore } from "../../../stores/RootStoreContext";
import {
  openAttachmentInSystem,
  revealAttachmentInDirectory,
} from "../../../native-bridge/nativeResource";
import { AttachmentPreviewDialog } from "../AttachmentPreviewDialog";
import { isPreviewableResource } from "../../resource-workspace/ResourceWorkspaceStore";
import {
  ResourceContextMenu,
  type ResourceMenuLocation,
} from "../../resource-workspace/ResourceContextMenu";
import { SystemContextDialog } from "../SystemContextDialog";
import { ContextRing, ContextSection } from "./ContextSection";
import { ContextSectionLayout } from "./ContextSectionLayout";
import { ContextPageControls } from "./ContextPageControls";
import { useContextPage, type ContextList } from "./useContextPage";
import {
  childStatusLabel,
  formatApprovalMode,
  formatBasisPoints,
  formatBytes,
  formatModelIdentity,
  formatNullableTokens,
  formatRunTime,
  formatTokens,
  formatVariant,
  runStatusLabel,
  sessionSkillRows,
  sessionStatusLabel,
} from "./contextDisplay";
import styles from "./index.module.scss";
import { SessionShellSetting } from "./SessionShellSetting";
import { ArtifactList } from "../../resource-workspace/ArtifactList";
import { BackgroundRunsSection } from "../BackgroundRunsSection";
import { BackgroundRunsStore } from "../BackgroundRunsSection/store";

type ContextSectionKey = "session" | "workspace" | "skills" | "attachments" | "artifacts" | "children" | "runs" | "background";

type ContextPanelProps = Readonly<{
  embedded?: boolean;
  visible?: boolean;
}>;

export const ContextPanel = observer(function ContextPanel(props: ContextPanelProps) {
  const PanelRoot = props.embedded ? "div" : "aside";
  const store = useRootStore();
  const application = store.projection.application;
  const session_id = store.navigation.selected_session_id;
  const session = store.projection.sessionSummary(session_id);
  const workspace = store.projection.workspaceSummary(session?.workspace_id);
  const session_view = session_id ? store.projection.session_views.get(session_id) : undefined;
  // 窗口占用由 Runtime 根据当前有效上下文计算，不能用旧 step 或累计用量取最大值。
  const context_usage = session_view?.usage.context ?? null;
  const context_prefix = context_usage?.accuracy === "estimated" ? "约 " : "";
  const session_workspace = session_view?.workspace;
  const draft_key = store.navigation.selected_draft_key;
  const new_session_draft = store.new_session_drafts.get(draft_key);
  const draft_workspace = store.projection.workspaceSummary(new_session_draft?.workspace_id);
  const [preview_attachment, setPreviewAttachment] = useState<AttachmentSummary | null>(null);
  const [attachment_menu, setAttachmentMenu] = useState<Readonly<{
    attachment: AttachmentSummary;
    location: ResourceMenuLocation;
  }> | null>(null);
  const [system_context, setSystemContext] = useState<SystemContextSnapshot | null>(null);
  const [system_context_error, setSystemContextError] = useState<string | null>(null);
  const [system_context_loading, setSystemContextLoading] = useState(false);
  const [locating_run, setLocatingRun] = useState<{ run_id: string } | null>(null);
  const [section_state, setSectionState] = useState<Record<string, Partial<Record<ContextSectionKey, boolean>>>>({});
  const [artifact_refresh, setArtifactRefresh] = useState(0);
  const [all_workspace_directories_visible, setAllWorkspaceDirectoriesVisible] = useState(false);
  const model = session?.model_selection ?? application?.model_settings.default_model ?? null;
  const section_owner = session_id ?? draft_key ?? "unselected";
  const sectionIsOpen = (section: ContextSectionKey) => section_state[section_owner]?.[section] ?? true;
  const toggleSection = (section: ContextSectionKey) => {
    setSectionState((current) => ({
      ...current,
      [section_owner]: {
        ...current[section_owner],
        [section]: !(current[section_owner]?.[section] ?? true),
      },
    }));
  };

  const paging_client = store.runtime_client;
  const panel_visible = props.visible ?? true;
  const is_stale = store.projection.is_stale;
  const background_session_id = new_session_draft ? null : session_id;
  // 列表和总数归当前会话。单双列切换会重新挂载区块，但沿用同一查询结果。
  const background = useMemo(() => paging_client && background_session_id
    ? new BackgroundRunsStore(paging_client, background_session_id)
    : null, [paging_client, background_session_id]);
  const background_available = panel_visible && !is_stale;

  useEffect(() => {
    if (!background) return;
    background.attach();
    const unsubscribe = store.subscribeBackgroundRuns(background.client, background.session_id, (event) => {
      if (event.type === "background_run_ended") background.ended(event.result.background_id, event.result);
      else background.changed(event.background_id);
    });
    return () => { unsubscribe(); background.dispose(); };
  }, [background, store]);

  useEffect(() => {
    if (!background) return;
    background.setAvailable(background_available);
    if (!background_available) return;
    const refresh = () => { void background.load(); };
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [background, background_available]);

  const generation = session_view?.conversation_generation ?? null;
  const section_can_read = (section: ContextList) => panel_visible && sectionIsOpen(section)
    && Boolean(session_view) && store.connection.state === "connected" && !is_stale;
  const runs = useContextPage("runs", paging_client, session_id, generation, section_can_read("runs"), async (client, session_id, page, signal) => {
    const { payload } = await client.command({ type: "list_runs", payload: { session_id, page, summary_only: true } }, { signal });
    return { rows: payload.runs, page: payload.page };
  });
  const children = useContextPage("children", paging_client, session_id, generation, section_can_read("children"), async (client, session_id, page, signal) => {
    const { payload } = await client.command({ type: "list_child_tasks", payload: { session_id, page } }, { signal });
    return { rows: payload.tasks, page: payload.page };
  });
  const attachment_page = useContextPage("attachments", paging_client, session_id, generation, section_can_read("attachments"), async (client, session_id, page, signal) => {
    const { payload } = await client.command({ type: "list_attachments", payload: { session_id, page } }, { signal });
    return { rows: payload.attachments, page: payload.page };
  });
  const artifacts = useContextPage("artifacts", paging_client, session_id, generation, section_can_read("artifacts"), async (client, session_id, page, signal) => {
    const { payload } = await client.command({ type: "list_artifacts", payload: { session_id, page } }, { signal });
    return { rows: payload.artifacts, page: payload.page };
  });
  const skills = useContextPage("skills", paging_client, session_id, generation, section_can_read("skills"), async (client, session_id, page, signal) => {
    const { payload } = await client.command({ type: "list_skills", payload: { session_id, page, include_plugins: true } }, { signal });
    if (!payload.page) throw new Error("Runtime 未返回技能分页。");
    if (!payload.snapshot.available) throw new Error("当前技能列表不可用，请重试");
    return { rows: sessionSkillRows({ active_skills: payload.active_skills ?? [] }, payload.snapshot), page: payload.page, diagnostic_count: payload.diagnostic_count };
  });
  const attachments = attachment_page.rows;

  const openAttachment = (attachment: AttachmentSummary) => {
    if (!session_id || !isPreviewableResource(attachment.original_name, attachment.media_type)) {
      setPreviewAttachment(attachment);
      return;
    }
    store.resource_workspace.openAttachment(`session:${session_id}`, attachment, attachments);
    if (!store.navigation.effective_right_sidebar_open) store.toggleRightSidebar();
  };

  const showAttachmentMenu = (
    attachment: AttachmentSummary,
    event: MouseEvent<HTMLElement> | KeyboardEvent<HTMLElement>,
  ) => {
    if ("key" in event && event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
    event.preventDefault();
    event.currentTarget.focus();
    const location = "clientX" in event
      ? { x: event.clientX, y: event.clientY }
      : (() => {
          const bounds = event.currentTarget.getBoundingClientRect();
          return { x: bounds.left + 16, y: bounds.bottom };
        })();
    setAttachmentMenu({ attachment, location });
  };

  const runAttachmentAction = (request: Promise<void>, fallback: string) => {
    void request.catch((failure: unknown) => {
      store.showInteractionError(failure instanceof Error ? failure.message : fallback);
    });
  };

  useEffect(() => {
    setSystemContext(null);
    setSystemContextError(null);
    setSystemContextLoading(false);
    setAllWorkspaceDirectoriesVisible(false);
    setAttachmentMenu(null);
  }, [session_id]);

  const locateRun = async (run_id: string) => {
    if (!session_id || locating_run?.run_id === run_id) return;
    const request = { run_id };
    setLocatingRun(request);
    store.navigation.closeChildTask();
    try {
      await store.locateConversationRun(session_id, run_id);
    } finally {
      setLocatingRun(current => current === request ? null : current);
    }
  };

  const openSystemContext = async () => {
    if (!session_id || system_context_loading) return;
    setSystemContextLoading(true);
    setSystemContextError(null);
    try {
      setSystemContext(await store.getSystemContext(session_id));
    } catch (error: unknown) {
      setSystemContextError(error instanceof Error ? error.message : "无法读取当前会话的系统上下文。");
    } finally {
      setSystemContextLoading(false);
    }
  };

  if (new_session_draft) {
    const draft_model = new_session_draft.model_selection ?? application?.model_settings.default_model ?? null;
    const draft_directories = draft_workspace
      ? [draft_workspace.user_directory ?? draft_workspace.agent_directory, ...draft_workspace.additional_directories]
      : [];
    return (
      <PanelRoot className={styles.panel} aria-label={props.embedded ? undefined : "当前上下文"} data-embedded={props.embedded || undefined}>
        {!props.embedded && <header className={styles.panel_header}>
          <h2>当前上下文</h2>
        </header>}
        <ContextSectionLayout>
          <ContextSection is_open={sectionIsOpen("session")} on_toggle={() => toggleSection("session")} title="草稿设置">
            <dl className={styles.definition_list}>
              <div><dt>模型</dt><dd>{formatModelIdentity(draft_model, application?.providers ?? [], Boolean(application?.model_settings.management))}</dd></div>
              <div><dt>执行方式</dt><dd>{formatVariant(new_session_draft.variant)} · {formatApprovalMode(new_session_draft.approval_mode)}</dd></div>
            </dl>
          </ContextSection>
          <ContextSection
            action={draft_workspace ? (
              <Tooltip content="编辑项目">
                <InlineIconButton
                  disabled={store.pending_workspace_action}
                  icon="edit"
                  label="编辑项目"
                  onClick={() => store.openWorkspaceEditor(draft_workspace.workspace_id)}
                />
              </Tooltip>
            ) : undefined}
            is_open={sectionIsOpen("workspace")}
            on_toggle={() => toggleSection("workspace")}
            title="工作区"
          >
            {draft_workspace ? (
              <div className={styles.workspace_directories}>
                {draft_directories.map((directory, index) => (
                  <div className={styles.path_row} key={directory} title={directory}>
                    <span aria-label={index === 0 ? "主目录" : undefined} className={styles.directory_icon} data-primary={index === 0 ? "true" : undefined}>
                      <Icon name="folder" size={16} />
                    </span>
                    <span className={styles.path_text}>{directory}</span>
                    <div className={styles.path_actions}>
                      {store.files.native_host && index === 0 && <InlineIconButton icon="external-link" label={`打开 ${directory}`} onClick={() => void store.openWorkspace(draft_workspace.workspace_id)} />}
                      <InlineIconButton icon="copy" label={`复制 ${directory}`} onClick={() => void store.copyWorkspacePath(directory)} />
                    </div>
                  </div>
                ))}
                <div className={styles.private_directory_row}>
                  <Icon name="folder" size={16} />
                  <span>会话私有目录</span>
                  <em>创建会话后可用</em>
                </div>
              </div>
            ) : (
              <div className={styles.private_directory_row}>
                <Icon name="folder" size={16} />
                <span>会话私有目录</span>
                <em>创建会话后可用</em>
              </div>
            )}
          </ContextSection>
        </ContextSectionLayout>
      </PanelRoot>
    );
  }

  return (
    <PanelRoot className={styles.panel} aria-label={props.embedded ? undefined : "当前上下文"} data-embedded={props.embedded || undefined}>
      {!props.embedded && <header className={styles.panel_header}>
        <h2>当前上下文</h2>
      </header>}
      <ContextSectionLayout>
        {session_view && <ContextSection
          action={artifacts.rows.length ? (
            <Tooltip content="刷新产物状态">
              <InlineIconButton icon="refresh" label="刷新产物状态" onClick={() => { artifacts.refresh(); setArtifactRefresh((value) => value + 1); }} />
            </Tooltip>
          ) : undefined}
          is_open={sectionIsOpen("artifacts")}
          on_toggle={() => toggleSection("artifacts")}
          title="产物"
        >
          <ArtifactList key={session_view.session.session_id} session_id={session_view.session.session_id} artifacts={artifacts.rows} refresh_key={artifact_refresh} />
          <ContextPageControls label="产物" page={artifacts} />
        </ContextSection>}
        <ContextSection is_open={sectionIsOpen("session")} on_toggle={() => toggleSection("session")} title="会话">
          {session ? (
            <dl className={styles.definition_list}>
              <div><dt>状态</dt><dd>{store.feedback.waiting(session.session_id) ? "等待回复" : sessionStatusLabel(
                session.lifecycle,
                store.live_execution.activeRunStatus(session.session_id, session_view?.active_run),
                session_view?.approvals.items.length ?? 0,
                session.resume_required,
              )}</dd></div>
              <div><dt>模型</dt><dd>{formatModelIdentity(model, application?.providers ?? [], Boolean(application?.model_settings.management))}</dd></div>
              <div><dt>图片理解</dt><dd>{imageHandlingLabel(session_view?.composer_capabilities.image_handling)}</dd></div>
              <div><dt>执行方式</dt><dd>{formatVariant(session.current_variant)} · {formatApprovalMode(session.approval_mode)}</dd></div>
              <div><dt>消息</dt><dd>{session.message_count}</dd></div>
            </dl>
          ) : <EmptyRow>尚未选择会话</EmptyRow>}
          {session_view && <SessionShellSetting key={session_view.session.session_id} view={session_view} />}
          {context_usage && (
            <div className={styles.context_usage}>
              <ContextRing basis_points={context_usage.usage_basis_points} />
              <div>
                <strong>上下文窗口 · {context_usage.mode === "server_compaction" ? "服务端压缩" : "本地摘要"}</strong>
                <span>{context_usage.used_tokens === null ? "占用未知" : `${context_prefix}${formatTokens(context_usage.used_tokens)}`} / {formatTokens(context_usage.window_tokens)}{context_usage.calibration_pending && " · 待校准"}</span>
              </div>
              <b>{context_usage.usage_basis_points === null ? "—" : `${context_prefix}${(context_usage.usage_basis_points / 100).toFixed(1)}%`}</b>
            </div>
          )}
          {session_view?.usage.previous_turn && (
            <dl className={`${styles.definition_list} ${styles.usage_list}`}>
              <div><dt>上一轮输入</dt><dd>{formatNullableTokens(session_view.usage.previous_turn.input_tokens)}</dd></div>
              <div><dt>上一轮输出</dt><dd>{formatNullableTokens(session_view.usage.previous_turn.output_tokens)}</dd></div>
              <div><dt>缓存命中</dt><dd>{formatNullableTokens(session_view.usage.previous_turn.cached_input_tokens)}</dd></div>
              <div><dt>最新命中率</dt><dd>{formatBasisPoints(session_view.usage.latest_cache_hit_basis_points)}</dd></div>
            </dl>
          )}
          {session_view?.usage.accumulated && (
            <dl className={`${styles.definition_list} ${styles.usage_list}`}>
              <div><dt>会话令牌</dt><dd>{formatNullableTokens(session_view.usage.accumulated.total_tokens)}</dd></div>
              <div><dt>综合命中率</dt><dd>{formatBasisPoints(session_view.usage.overall_cache_hit_basis_points)}</dd></div>
            </dl>
          )}
          {session && (
            <button disabled={store.remote_device} title={store.remote_device ? "组网设备暂不支持查看系统上下文" : undefined} className={styles.system_context_row} onClick={() => void openSystemContext()} type="button">
              <strong>系统上下文</strong>
              <em>{system_context_loading ? "读取中…" : "查看原文"}</em>
            </button>
          )}
          {system_context_error && <p className={styles.context_error}>{system_context_error}</p>}
        </ContextSection>
        <ContextSection
          action={workspace ? (
            <Tooltip content="编辑项目">
              <InlineIconButton
                disabled={store.pending_workspace_action}
                icon="edit"
                label="编辑项目"
                onClick={() => store.openWorkspaceEditor(workspace.workspace_id)}
              />
            </Tooltip>
          ) : undefined}
          is_open={sectionIsOpen("workspace")}
          on_toggle={() => toggleSection("workspace")}
          title="工作区"
        >
          {session ? (
            <>
              {workspace && session_workspace ? (
                <>
                  <div className={styles.workspace_directories}>
                    {[session_workspace.primary_directory, ...session_workspace.additional_directories]
                      .slice(0, all_workspace_directories_visible ? undefined : 3)
                      .map((directory, index) => {
                        const locator = sessionWorkspaceLocator(index);
                        return (
                          <WorkspaceDirectoryRow
                            key={directory}
                            label={directory}
                            on_copy={() => void store.copyWorkspacePath(directory)}
                            on_open_system={store.files.native_host ? () => void store.openSessionWorkspaceDirectory(session.session_id, index) : undefined}
                            on_open_tab={() => store.resource_workspace.openWorkspace(`session:${session.session_id}`, locator)}
                            primary={index === 0}
                          />
                        );
                      })}
                    {!all_workspace_directories_visible && session_workspace.additional_directories.length > 2 && (
                      <Button className={styles.more_directories} onClick={() => setAllWorkspaceDirectoriesVisible(true)} size="small" variant="text">
                        显示其余 {session_workspace.additional_directories.length - 2} 条
                      </Button>
                    )}
                    <WorkspaceDirectoryRow
                      label="会话私有目录"
                      on_copy={() => void store.copySessionResourcePath(session.session_id, SESSION_PRIVATE_ROOT)}
                      on_open_system={store.files.native_host ? () => void store.openSessionResourceInSystem(session.session_id, SESSION_PRIVATE_ROOT) : undefined}
                      on_open_tab={() => store.resource_workspace.openWorkspace(
                        `session:${session.session_id}`,
                        SESSION_PRIVATE_ROOT,
                      )}
                    />
                  </div>
                  {!session_workspace.directories_match_current && <p className={styles.workspace_changed_note}>本会话继续使用创建时的工作目录。</p>}
                </>
              ) : (
                <>
                  <EmptyRow>未绑定项目</EmptyRow>
                  <WorkspaceDirectoryRow
                    label="会话私有目录"
                    on_copy={() => void store.copySessionResourcePath(session.session_id, SESSION_PRIVATE_ROOT)}
                    on_open_system={store.files.native_host ? () => void store.openSessionResourceInSystem(session.session_id, SESSION_PRIVATE_ROOT) : undefined}
                    on_open_tab={() => store.resource_workspace.openWorkspace(
                      `session:${session.session_id}`,
                      SESSION_PRIVATE_ROOT,
                    )}
                  />
                </>
              )}
            </>
          ) : <EmptyRow>尚未选择会话</EmptyRow>}
        </ContextSection>
        <ContextSection
          is_open={sectionIsOpen("skills")}
          on_toggle={() => toggleSection("skills")}
          title="技能"
        >
          {session_view || new_session_draft ? (
            <div className={styles.skill_context}>
              {skills.rows.length > 0 ? (
                <div className={styles.skill_list}>
                  {skills.rows.map((skill) => (
                    <div key={skill.name}>
                      <strong title={skill.plugin_name ? `${skill.plugin_name} · ${skill.name}` : skill.name}>{skill.plugin_name ? `${skill.plugin_name} · ${skill.name}` : skill.name}</strong>
                      <span>{skill.status_label}</span>
                    </div>
                  ))}
                </div>
              ) : <EmptyRow>{skills.loading ? "正在读取技能" : "当前没有可用技能"}</EmptyRow>}
              {skills.diagnostics > 0 && (
                <Button className={styles.skill_diagnostics} onClick={() => store.settings.open("skills")} size="small" variant="text">
                  查看 {skills.diagnostics} 项技能诊断
                </Button>
              )}
            </div>
          ) : <EmptyRow>尚未选择会话</EmptyRow>}
          <ContextPageControls label="技能" page={skills} />
        </ContextSection>
        <ContextSection
          is_open={sectionIsOpen("attachments")}
          on_toggle={() => toggleSection("attachments")}
          title={`会话附件 · ${attachment_page.total}`}
        >
          {attachments.length > 0 ? (
            <div className={styles.attachment_rows}>
              {attachments.map((attachment) => (
                <button
                  disabled={attachment.state !== "ready"}
                  key={attachment.attachment_id}
                  onClick={() => openAttachment(attachment)}
                  onContextMenu={(event) => showAttachmentMenu(attachment, event)}
                  onKeyDown={(event) => showAttachmentMenu(attachment, event)}
                  title={attachment.original_name}
                  type="button"
                >
                  <Icon name="paperclip" size={14} />
                  <span>{attachment.original_name}</span>
                  <small>{attachment.state === "ready" ? formatBytes(attachment.size_bytes) : "不可用"}</small>
                </button>
              ))}
            </div>
          ) : <EmptyRow>{attachment_page.loading ? "正在读取会话附件" : "暂无会话附件"}</EmptyRow>}
          <ContextPageControls label="会话附件" page={attachment_page} />
        </ContextSection>
        {session_id && session_view && (
          <ContextSection
            is_open={sectionIsOpen("children")}
            on_toggle={() => toggleSection("children")}
            title={`子任务 · ${children.total}`}
          >
            <div className={styles.child_task_rows}>
              {children.rows.map((task) => (
                <button
                  data-selected={store.navigation.selected_child_task_id === task.child_task_id}
                  key={task.child_task_id}
                  onClick={() => void store.openChildTask(session_id, task.child_task_id)}
                  title={task.title}
                  type="button"
                >
                  <i data-status={task.status} />
                  <span>{task.title}</span>
                  <small>{session_view.approvals.items.some((approval) => approval.child_task_id === task.child_task_id) ? "待审批" : childStatusLabel(task.status)}</small>
                </button>
              ))}
            </div>
            {children.rows.length === 0 && <EmptyRow>{children.loading ? "正在读取子任务" : "暂无子任务"}</EmptyRow>}
            <ContextPageControls label="子任务" page={children} />
          </ContextSection>
        )}
        {background && <BackgroundRunsSection
          key={JSON.stringify([store.navigation.target_device, background.session_id])}
          is_stale={is_stale}
          is_open={sectionIsOpen("background")}
          on_toggle={() => toggleSection("background")}
          runs={background}
          visible={panel_visible}
        />}
        <ContextSection
          is_open={sectionIsOpen("runs")}
          on_toggle={() => toggleSection("runs")}
          title={`运行记录 · ${runs.total}`}
        >
          {runs.rows.length > 0 ? (
            <div className={styles.run_rows}>
              {runs.rows.map((run) => (
                <button
                  disabled={locating_run?.run_id === run.run_id}
                  key={run.run_id}
                  onClick={() => void locateRun(run.run_id)}
                  type="button"
                >
                  <i data-status={run.status} />
                  <span>
                    <strong>运行 #{run.attempt} · {formatVariant(run.variant)}</strong>
                    <small>{formatRunTime(run.finished_at_ms ?? run.created_at_ms)} · {run.tools?.length ?? 0} 个工具</small>
                  </span>
                  <em>{locating_run?.run_id === run.run_id ? "定位中…" : runStatusLabel(run.status)}</em>
                </button>
              ))}
            </div>
          ) : <EmptyRow>还没有运行记录</EmptyRow>}
          <ContextPageControls label="运行记录" page={runs} />
        </ContextSection>
      </ContextSectionLayout>
      {attachment_menu && (
        <ResourceContextMenu
          items={[
            {
              disabled: !isPreviewableResource(
                attachment_menu.attachment.original_name,
                attachment_menu.attachment.media_type,
              ),
              label: "在资源栏打开",
              on_select: () => openAttachment(attachment_menu.attachment),
            },
            {label:"下载文件",on_select:()=>runAttachmentAction(store.files.downloadAttachment(attachment_menu.attachment.session_id, attachment_menu.attachment.attachment_id, attachment_menu.attachment.original_name),"下载失败。")},
            ...(store.files.native_host ? [
            {
              label: "使用系统应用打开",
              on_select: () => runAttachmentAction(
                openAttachmentInSystem(
                  attachment_menu.attachment.session_id,
                  attachment_menu.attachment.attachment_id,
                ),
                "无法使用系统应用打开。",
              ),
            },
            {
              label: revealFileLabel(),
              on_select: () => runAttachmentAction(
                revealAttachmentInDirectory(
                  attachment_menu.attachment.session_id,
                  attachment_menu.attachment.attachment_id,
                ),
                "无法在文件管理器中显示。",
              ),
            }            ] : []),
          ]}
          location={attachment_menu.location}
          on_close={() => setAttachmentMenu(null)}
        />
      )}
      <PresenceBoundary present={preview_attachment !== null}>
      {preview_attachment && (
        <AttachmentPreviewDialog
          attachment={preview_attachment}
          on_close={() => setPreviewAttachment(null)}
        />
      )}
      </PresenceBoundary>
      <PresenceBoundary present={system_context !== null}>
      {system_context && (
        <SystemContextDialog on_close={() => setSystemContext(null)} snapshot={system_context} />
      )}
      </PresenceBoundary>
    </PanelRoot>
  );
});

const SESSION_PRIVATE_ROOT: SessionResourceLocator = {
  root: { type: "session_private" },
  relative_path: "",
};

function sessionWorkspaceLocator(directory_index: number): SessionResourceLocator {
  return directory_index === 0
    ? { root: { type: "workspace_primary" }, relative_path: "" }
    : { root: { type: "workspace_additional", directory_index: directory_index - 1 }, relative_path: "" };
}

function WorkspaceDirectoryRow(props: Readonly<{
  label: string;
  on_copy: () => void;
  on_open_system?: () => void;
  on_open_tab: () => void;
  primary?: boolean;
}>) {
  return (
    <div className={styles.path_row}>
      <button aria-label={`浏览 ${props.label}`} className={styles.path_open} onClick={props.on_open_tab} type="button">
        <span
          aria-label={props.primary ? "主目录" : undefined}
          className={styles.directory_icon}
          data-primary={props.primary || undefined}
          role={props.primary ? "img" : undefined}
        >
          <Icon name="folder" size={16} />
        </span>
        <span className={styles.path_text}>{props.label}</span>
      </button>
      <div className={styles.path_actions}>
        {props.on_open_system && <InlineIconButton icon="external-link" label={`打开 ${props.label}`} onClick={props.on_open_system} />}
        <InlineIconButton icon="copy" label={`复制 ${props.label}`} onClick={props.on_copy} />
      </div>
    </div>
  );
}

function imageHandlingLabel(mode: SessionViewSnapshot["composer_capabilities"]["image_handling"] | undefined): string {
  if (mode === "native") return "模型原生";
  if (mode === "tool") return "辅助视觉模型";
  return "当前不可用";
}
