import { observer } from "mobx-react-lite";
import { useEffect, useState, type KeyboardEvent, type MouseEvent } from "react";
import type {
  AttachmentSummary,
  SessionResourceLocator,
  SessionViewSnapshot,
  SystemContextSnapshot,
} from "../../../generated/assistant-protocol";
import { Button } from "../../../components/Button";
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

type ContextSectionKey = "session" | "workspace" | "skills" | "attachments" | "children" | "runs";

type ContextPanelProps = Readonly<{
  embedded?: boolean;
}>;

export const ContextPanel = observer(function ContextPanel(props: ContextPanelProps) {
  const PanelRoot = props.embedded ? "div" : "aside";
  const store = useRootStore();
  const application = store.projection.application;
  const session_id = store.navigation.selected_session_id;
  const session = application?.active_sessions.find((item) => item.session_id === session_id)
    ?? application?.archived_sessions.find((item) => item.session_id === session_id);
  const workspace = application?.workspaces.find((item) => item.workspace_id === session?.workspace_id);
  const session_view = session_id ? store.projection.session_views.get(session_id) : undefined;
  const skills = sessionSkillRows(session_view);
  const session_workspace = session_view?.workspace;
  const draft_key = store.navigation.selected_draft_key;
  const new_session_draft = store.new_session_drafts.get(draft_key);
  const draft_workspace = application?.workspaces.find(
    (item) => item.workspace_id === new_session_draft?.workspace_id,
  );
  const attachments = session_view?.attachments ?? [];
  const [preview_attachment, setPreviewAttachment] = useState<AttachmentSummary | null>(null);
  const [attachment_menu, setAttachmentMenu] = useState<Readonly<{
    attachment: AttachmentSummary;
    location: ResourceMenuLocation;
  }> | null>(null);
  const [system_context, setSystemContext] = useState<SystemContextSnapshot | null>(null);
  const [system_context_error, setSystemContextError] = useState<string | null>(null);
  const [system_context_loading, setSystemContextLoading] = useState(false);
  const [locating_run_id, setLocatingRunId] = useState<string | null>(null);
  const [section_state, setSectionState] = useState<Record<string, Partial<Record<ContextSectionKey, boolean>>>>({});
  const [all_workspace_directories_visible, setAllWorkspaceDirectoriesVisible] = useState(false);
  const model = application?.models.find((item) => item.model_key === session?.model_key);
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
    if (!session_id || locating_run_id) {
      return;
    }
    setLocatingRunId(run_id);
    store.navigation.closeChildTask();
    await store.locateConversationRun(session_id, run_id);
    setLocatingRunId(null);
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
    const draft_model = application?.models.find((item) => item.model_key === new_session_draft.model_key);
    const draft_directories = draft_workspace
      ? [draft_workspace.user_directory, ...draft_workspace.additional_directories]
      : [];
    return (
      <PanelRoot className={styles.panel} aria-label={props.embedded ? undefined : "当前上下文"} data-embedded={props.embedded || undefined}>
        {!props.embedded && <header className={styles.panel_header}>
          <h2>当前上下文</h2>
        </header>}
        <ContextSectionLayout>
          <ContextSection is_open={sectionIsOpen("session")} on_toggle={() => toggleSection("session")} title="草稿设置">
            <dl className={styles.definition_list}>
              <div><dt>模型</dt><dd>{formatModelIdentity(draft_model?.display_name, new_session_draft.model_key)}</dd></div>
              <div><dt>执行方式</dt><dd>{formatVariant(new_session_draft.variant)} · {formatApprovalMode(new_session_draft.approval_mode)}</dd></div>
            </dl>
          </ContextSection>
          <ContextSection
            action={draft_workspace ? (
              <Tooltip content="编辑工作空间">
                <InlineIconButton
                  disabled={store.pending_workspace_action}
                  icon="edit"
                  label="编辑工作空间"
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
                      {index === 0 && <InlineIconButton icon="external-link" label={`打开 ${directory}`} onClick={() => void store.openWorkspace(draft_workspace.workspace_id)} />}
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
        <ContextSection is_open={sectionIsOpen("session")} on_toggle={() => toggleSection("session")} title="会话">
          {session ? (
            <dl className={styles.definition_list}>
              <div><dt>状态</dt><dd>{sessionStatusLabel(
                session.lifecycle,
                session_view?.active_run?.status,
                session_view?.approvals.items.length ?? 0,
                session.resume_required,
              )}</dd></div>
              <div><dt>模型</dt><dd>{formatModelIdentity(model?.display_name, session.model_key)}</dd></div>
              <div><dt>图片理解</dt><dd>{imageHandlingLabel(session_view?.composer_capabilities.image_handling)}</dd></div>
              <div><dt>执行方式</dt><dd>{formatVariant(session.current_variant)} · {formatApprovalMode(session.approval_mode)}</dd></div>
              <div><dt>消息</dt><dd>{session.message_count}</dd></div>
            </dl>
          ) : <p className={styles.empty_row}>尚未选择会话</p>}
          {session_view?.usage.context && (
            <div className={styles.context_usage}>
              <ContextRing basis_points={session_view.usage.context.usage_basis_points} />
              <div>
                <strong>上下文窗口</strong>
                <span>{formatTokens(session_view.usage.context.used_tokens)} / {formatTokens(session_view.usage.context.window_tokens)}</span>
              </div>
              <b>{(session_view.usage.context.usage_basis_points / 100).toFixed(1)}%</b>
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
            <button className={styles.system_context_row} onClick={() => void openSystemContext()} type="button">
              <strong>系统上下文</strong>
              <em>{system_context_loading ? "读取中…" : "查看原文"}</em>
            </button>
          )}
          {system_context_error && <p className={styles.context_error}>{system_context_error}</p>}
        </ContextSection>
        <ContextSection
          action={workspace ? (
            <Tooltip content="编辑工作空间">
              <InlineIconButton
                disabled={store.pending_workspace_action}
                icon="edit"
                label="编辑工作空间"
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
                            on_open_system={() => void store.openSessionWorkspaceDirectory(session.session_id, index)}
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
                      on_open_system={() => void store.openSessionResourceInSystem(session.session_id, SESSION_PRIVATE_ROOT)}
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
                  <p className={styles.empty_row}>未绑定工作空间</p>
                  <WorkspaceDirectoryRow
                    label="会话私有目录"
                    on_copy={() => void store.copySessionResourcePath(session.session_id, SESSION_PRIVATE_ROOT)}
                    on_open_system={() => void store.openSessionResourceInSystem(session.session_id, SESSION_PRIVATE_ROOT)}
                    on_open_tab={() => store.resource_workspace.openWorkspace(
                      `session:${session.session_id}`,
                      SESSION_PRIVATE_ROOT,
                    )}
                  />
                </>
              )}
            </>
          ) : <p className={styles.empty_row}>尚未选择会话</p>}
        </ContextSection>
        <ContextSection
          is_open={sectionIsOpen("skills")}
          on_toggle={() => toggleSection("skills")}
          title="技能"
        >
          {session_view ? (
            <div className={styles.skill_context}>
              {skills.length > 0 ? (
                <div className={styles.skill_list}>
                  {skills.map((skill) => (
                    <div key={skill.name}>
                      <strong title={skill.name}>{skill.name}</strong>
                      <span>{skill.status_label}</span>
                    </div>
                  ))}
                </div>
              ) : <p className={styles.empty_row}>{emptySkillMessage(session_view)}</p>}
              {(session_view.skill_catalog?.diagnostics.length ?? 0) > 0 && (
                <Button className={styles.skill_diagnostics} onClick={() => store.settings.open("skills")} size="small" variant="text">
                  查看 {session_view.skill_catalog?.diagnostics.length} 项技能诊断
                </Button>
              )}
            </div>
          ) : <p className={styles.empty_row}>尚未选择会话</p>}
        </ContextSection>
        <ContextSection
          is_open={sectionIsOpen("attachments")}
          on_toggle={() => toggleSection("attachments")}
          title={`会话附件 · ${attachments.length}`}
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
          ) : <p className={styles.empty_row}>暂无会话附件</p>}
        </ContextSection>
        {session_id && session_view && session_view.child_tasks.length > 0 && (
          <ContextSection
            is_open={sectionIsOpen("children")}
            on_toggle={() => toggleSection("children")}
            title={`子任务 · ${session_view.child_tasks.length}`}
          >
            <div className={styles.child_task_rows}>
              {session_view.child_tasks.map((item) => (
                <button
                  data-selected={store.navigation.selected_child_task_id === item.task.child_task_id}
                  key={item.task.child_task_id}
                  onClick={() => void store.openChildTask(session_id, item.task.child_task_id)}
                  title={item.task.title}
                  type="button"
                >
                  <i data-status={item.task.status} />
                  <span>{item.task.title}</span>
                  <small>{item.pending_approval_count > 0 ? "待审批" : childStatusLabel(item.task.status)}</small>
                </button>
              ))}
            </div>
          </ContextSection>
        )}
        <ContextSection
          is_open={sectionIsOpen("runs")}
          on_toggle={() => toggleSection("runs")}
          title={`运行记录 · ${session_view?.runs.length ?? 0}`}
        >
          {session_view && session_view.runs.length > 0 ? (
            <div className={styles.run_rows}>
              {[...session_view.runs].reverse().map((run) => (
                <button
                  disabled={locating_run_id !== null}
                  key={run.run_id}
                  onClick={() => void locateRun(run.run_id)}
                  type="button"
                >
                  <i data-status={run.status} />
                  <span>
                    <strong>运行 #{run.attempt} · {formatVariant(run.variant)}</strong>
                    <small>{formatRunTime(run.finished_at_ms ?? run.created_at_ms)} · {run.tools?.length ?? 0} 个工具</small>
                  </span>
                  <em>{locating_run_id === run.run_id ? "定位中…" : runStatusLabel(run.status)}</em>
                </button>
              ))}
            </div>
          ) : <p className={styles.empty_row}>还没有运行记录</p>}
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
              label: "在 Finder 中显示",
              on_select: () => runAttachmentAction(
                revealAttachmentInDirectory(
                  attachment_menu.attachment.session_id,
                  attachment_menu.attachment.attachment_id,
                ),
                "无法在 Finder 中显示。",
              ),
            },
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
  on_open_system: () => void;
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
        <InlineIconButton icon="external-link" label={`打开 ${props.label}`} onClick={props.on_open_system} />
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

function emptySkillMessage(view: SessionViewSnapshot): string {
  if (!view.skill_catalog || view.skill_catalog.status === "legacy_unavailable") {
    return "此历史会话没有可展示的技能信息";
  }
  if (view.skill_catalog.status === "unavailable") return "当前会话的技能信息不可用";
  return "当前会话没有可用技能";
}
