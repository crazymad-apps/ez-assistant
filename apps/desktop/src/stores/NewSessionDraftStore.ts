import { action, makeObservable, observable } from "mobx";
import type {
  AgentVariant,
  ApprovalMode,
  CustomAgentSelection,
  AttachmentId,
  ModelSelection,
  McpSelectionTagSnapshot,
  QuotedTextSnapshot,
  ReasoningEffortKey,
  SessionMaterializationManifest,
  SkillSummarySnapshot,
  WorkspaceId,
} from "@ez-assistant/protocol";

export type NewSessionDraftKey = `workspace:${string}` | "unbound";

export type ComposerAttachment = Readonly<{
  selection_id: string;
  original_name: string;
  size_bytes: number;
  media_type: string | null;
  origin: "file_picker" | "clipboard";
  state: "selected" | "uploading" | "uploaded" | "failed";
  attachment_id: AttachmentId | null;
  error: string | null;
  operation_id: string | null;
}>;

export type NewSessionDraft = Readonly<{
  key: NewSessionDraftKey;
  workspace_id: WorkspaceId | null;
  text: string;
  attachments: readonly ComposerAttachment[];
  quotes: readonly QuotedTextSnapshot[];
  model_selection: ModelSelection | null;
  reasoning_effort: ReasoningEffortKey | null;
  variant: AgentVariant;
  approval_mode: ApprovalMode;
  goal_armed: boolean;
  selected_skill_name: string | null;
  selected_agent: CustomAgentSelection | null;
  selected_mcp: McpSelectionTagSnapshot | null;
  selected_mcp_availability: "unverified" | "available" | "unavailable";
  skill_options: readonly SkillSummarySnapshot[];
  skill_status: "idle" | "loading" | "ready" | "failed";
  materialization_attempt: SessionMaterializationManifest | null;
}>;

export function draftKeyForWorkspace(workspace_id: WorkspaceId | null): NewSessionDraftKey {
  return workspace_id ? `workspace:${workspace_id}` : "unbound";
}

export function workspaceForDraftKey(key: NewSessionDraftKey): WorkspaceId | null {
  return key === "unbound" ? null : key.slice("workspace:".length);
}

/**
 * 保存当前 WebView 进程内的新会话编辑状态。草稿没有 Session 身份，也不写入偏好或 Runtime；
 * 所有更新都替换单个不可变快照，避免组件与 Store 同时持有可写副本。
 */
export class NewSessionDraftStore {
  readonly drafts = observable.map<NewSessionDraftKey, NewSessionDraft>(undefined, { deep: false });
  default_approval_mode: ApprovalMode = "auto";
  default_model_selection: ModelSelection | null = null;

  constructor() {
    makeObservable(this, {
      drafts: observable,
      default_approval_mode: observable,
      default_model_selection: observable,
      applyDefaults: action,
      discardUnavailableDefaultModel: action,
      open: action,
      updateText: action,
      updateAttachments: action,
      updateModel: action,
      updateReasoningEffort: action,
      updateVariant: action,
      updateApprovalMode: action,
      updateGoalArmed: action,
      updateSelectedSkill: action,
      updateSelectedAgent: action,
      updateSelectedMcp: action,
      setSelectedMcpAvailability: action,
      beginSkillLoad: action,
      applySkillOptions: action,
      failSkillLoad: action,
      beginMaterialization: action,
      clearMaterializationAttempt: action,
      move: action,
      setAttachmentTransferState: action,
      remove: action,
      clear: action,
    });
  }

  applyDefaults(approval_mode: ApprovalMode, model_selection: ModelSelection | null): void {
    this.default_approval_mode = approval_mode;
    this.default_model_selection = cloneModelSelection(model_selection);
  }

  discardUnavailableDefaultModel(selection: ModelSelection): void {
    if (!sameModelSelection(this.default_model_selection, selection)) return;
    this.default_model_selection = null;
    for (const [key, draft] of this.drafts) {
      if (!isDraftMeaningful(draft)
        && draft.materialization_attempt === null
        && sameModelSelection(draft.model_selection, selection)) {
        this.drafts.set(key, { ...draft, model_selection: null, reasoning_effort: null });
      }
    }
  }

  open(key: NewSessionDraftKey): NewSessionDraft {
    const existing = this.drafts.get(key);
    if (existing) return existing;
    const created = createDraft(key, this.default_approval_mode, this.default_model_selection);
    this.drafts.set(key, created);
    return created;
  }

  get(key: NewSessionDraftKey | null): NewSessionDraft | null {
    return key ? this.drafts.get(key) ?? null : null;
  }

  hasDraft(key: NewSessionDraftKey): boolean {
    const draft = this.drafts.get(key);
    return draft ? isDraftMeaningful(draft) || draft.materialization_attempt !== null : false;
  }

  move(source: NewSessionDraftKey, target: NewSessionDraftKey): boolean {
    if (source === target) return true;
    const draft = this.drafts.get(source);
    const destination = this.drafts.get(target);
    if (!draft || draft.materialization_attempt || destination?.materialization_attempt
      || (destination && isDraftCustomized(destination, this.default_approval_mode, this.default_model_selection))) return false;
    this.drafts.delete(source);
    this.drafts.set(target, {
      ...draft, key: target, workspace_id: workspaceForDraftKey(target),
      skill_options: [], skill_status: "idle", selected_mcp_availability: "unverified",
    });
    return true;
  }

  updateText(key: NewSessionDraftKey, text: string): void {
    this.#patch(key, { text });
  }

  updateAttachments(key: NewSessionDraftKey, attachments: readonly ComposerAttachment[]): void {
    this.#patch(key, { attachments });
  }

  updateModel(key: NewSessionDraftKey, model_selection: ModelSelection | null): void {
    this.#patch(key, { model_selection, reasoning_effort: null });
  }

  updateReasoningEffort(key: NewSessionDraftKey, reasoning_effort: ReasoningEffortKey | null): void {
    this.#patch(key, { reasoning_effort });
  }

  updateVariant(key: NewSessionDraftKey, variant: AgentVariant): void {
    const draft = this.drafts.get(key);
    if (draft?.variant !== variant) this.#patch(key, { variant, selected_mcp: null });
  }

  updateApprovalMode(key: NewSessionDraftKey, approval_mode: ApprovalMode): void {
    this.#patch(key, { approval_mode });
  }

  updateGoalArmed(key: NewSessionDraftKey, goal_armed: boolean): void {
    this.#patch(key, { goal_armed });
  }

  updateSelectedSkill(key: NewSessionDraftKey, selected_skill_name: string | null): void {
    this.#patch(key, { selected_skill_name });
  }

  updateSelectedAgent(key: NewSessionDraftKey, selected_agent: CustomAgentSelection | null): void {
    this.#patch(key, { selected_agent });
  }

  updateSelectedMcp(key: NewSessionDraftKey, selected_mcp: McpSelectionTagSnapshot | null): void {
    this.#patch(key, { selected_mcp, selected_mcp_availability: "available" });
  }

  setSelectedMcpAvailability(key: NewSessionDraftKey, server_key: string, available: boolean): void {
    const draft = this.drafts.get(key);
    if (!draft || draft.materialization_attempt || draft.selected_mcp?.server_key !== server_key) return;
    this.drafts.set(key, { ...draft, selected_mcp_availability: available ? "available" : "unavailable" });
  }

  beginSkillLoad(key: NewSessionDraftKey): boolean {
    const draft = this.drafts.get(key);
    if (!draft || draft.skill_status === "loading" || draft.skill_status === "ready") return false;
    this.drafts.set(key, { ...draft, skill_status: "loading" });
    return true;
  }

  applySkillOptions(key: NewSessionDraftKey, skill_options: readonly SkillSummarySnapshot[]): void {
    const draft = this.drafts.get(key);
    if (!draft) return;
    this.drafts.set(key, { ...draft, skill_options, skill_status: "ready" });
  }

  failSkillLoad(key: NewSessionDraftKey): void {
    const draft = this.drafts.get(key);
    if (draft) this.drafts.set(key, { ...draft, skill_status: "failed" });
  }

  beginMaterialization(key: NewSessionDraftKey, manifest: SessionMaterializationManifest): void {
    const draft = this.drafts.get(key);
    if (!draft) return;
    this.drafts.set(key, { ...draft, materialization_attempt: manifest });
  }

  clearMaterializationAttempt(key: NewSessionDraftKey): void {
    const draft = this.drafts.get(key);
    if (draft?.materialization_attempt) {
      this.drafts.set(key, { ...draft, materialization_attempt: null });
    }
  }

  setAttachmentTransferState(
    key: NewSessionDraftKey,
    state: "selected" | "uploading" | "failed",
    error: string | null = null,
  ): void {
    const draft = this.drafts.get(key);
    if (!draft) return;
    this.drafts.set(key, {
      ...draft,
      attachments: draft.attachments.map((attachment) => ({
        ...attachment,
        state,
        error,
        operation_id: null,
      })),
    });
  }

  remove(key: NewSessionDraftKey): NewSessionDraft | null {
    const draft = this.drafts.get(key) ?? null;
    this.drafts.delete(key);
    return draft;
  }

  clear(): readonly NewSessionDraft[] {
    const drafts = [...this.drafts.values()];
    this.drafts.clear();
    return drafts;
  }

  #patch(key: NewSessionDraftKey, patch: Partial<NewSessionDraft>): void {
    const draft = this.drafts.get(key);
    if (!draft || draft.materialization_attempt) return;
    this.drafts.set(key, { ...draft, ...patch });
  }
}

export function isDraftMeaningful(draft: NewSessionDraft): boolean {
  return Boolean(
    draft.text.trim()
    || draft.attachments.length
    || draft.quotes.length
    || draft.goal_armed
    || draft.selected_skill_name
    || draft.selected_agent
    || draft.selected_mcp,
  );
}

export function isDraftCustomized(
  draft: NewSessionDraft,
  default_approval_mode: ApprovalMode = "auto",
  default_model_selection: ModelSelection | null = null,
): boolean {
  return isDraftMeaningful(draft)
    || draft.materialization_attempt !== null
    || !sameModelSelection(draft.model_selection, default_model_selection)
    || draft.reasoning_effort !== null
    || draft.variant !== "build"
    || draft.approval_mode !== default_approval_mode;
}

function createDraft(
  key: NewSessionDraftKey,
  approval_mode: ApprovalMode,
  model_selection: ModelSelection | null,
): NewSessionDraft {
  return {
    key,
    workspace_id: workspaceForDraftKey(key),
    text: "",
    attachments: [],
    quotes: [],
    model_selection: cloneModelSelection(model_selection),
    reasoning_effort: null,
    variant: "build",
    approval_mode,
    goal_armed: false,
    selected_skill_name: null,
    selected_agent: null,
    selected_mcp: null,
    selected_mcp_availability: "available",
    skill_options: [],
    skill_status: "idle",
    materialization_attempt: null,
  };
}

function sameModelSelection(left: ModelSelection | null, right: ModelSelection | null): boolean {
  return left === null
    ? right === null
    : right !== null
      && left.provider_instance_id === right.provider_instance_id
      && left.model_id === right.model_id;
}

function cloneModelSelection(selection: ModelSelection | null): ModelSelection | null {
  return selection ? { ...selection } : null;
}
