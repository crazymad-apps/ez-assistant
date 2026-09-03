import { action, makeObservable, observable } from "mobx";
import type {
  AgentVariant,
  ApprovalMode,
  AttachmentId,
  ModelKey,
  McpSelectionTagSnapshot,
  QuotedTextSnapshot,
  ReasoningEffortKey,
  SessionMaterializationManifest,
  SkillSummarySnapshot,
  WorkspaceId,
} from "../generated/assistant-protocol";

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
  model_key: ModelKey | null;
  reasoning_effort: ReasoningEffortKey | null;
  variant: AgentVariant;
  approval_mode: ApprovalMode;
  goal_armed: boolean;
  selected_skill_name: string | null;
  selected_mcp: McpSelectionTagSnapshot | null;
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

  constructor() {
    makeObservable(this, {
      drafts: observable,
      open: action,
      updateText: action,
      updateAttachments: action,
      updateModel: action,
      updateReasoningEffort: action,
      updateVariant: action,
      updateApprovalMode: action,
      updateGoalArmed: action,
      updateSelectedSkill: action,
      updateSelectedMcp: action,
      beginSkillLoad: action,
      applySkillOptions: action,
      failSkillLoad: action,
      beginMaterialization: action,
      clearMaterializationAttempt: action,
      setAttachmentTransferState: action,
      remove: action,
      clear: action,
    });
  }

  open(key: NewSessionDraftKey, default_model_key: ModelKey | null): NewSessionDraft {
    const existing = this.drafts.get(key);
    if (existing) return existing;
    const created = createDraft(key, default_model_key);
    this.drafts.set(key, created);
    return created;
  }

  get(key: NewSessionDraftKey | null): NewSessionDraft | null {
    return key ? this.drafts.get(key) ?? null : null;
  }

  hasDraft(key: NewSessionDraftKey): boolean {
    const draft = this.drafts.get(key);
    return draft ? isDraftMeaningful(draft) : false;
  }

  updateText(key: NewSessionDraftKey, text: string): void {
    this.#patch(key, { text });
  }

  updateAttachments(key: NewSessionDraftKey, attachments: readonly ComposerAttachment[]): void {
    this.#patch(key, { attachments });
  }

  updateModel(key: NewSessionDraftKey, model_key: ModelKey | null): void {
    this.#patch(key, { model_key, reasoning_effort: null });
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

  updateSelectedMcp(key: NewSessionDraftKey, selected_mcp: McpSelectionTagSnapshot | null): void {
    this.#patch(key, { selected_mcp });
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
    const selected_skill_name = skill_options.some((skill) => skill.name === draft.selected_skill_name)
      ? draft.selected_skill_name
      : null;
    this.drafts.set(key, { ...draft, skill_options, skill_status: "ready", selected_skill_name });
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
    if (!draft) return;
    this.drafts.set(key, { ...draft, ...patch, materialization_attempt: null });
  }
}

export function isDraftMeaningful(draft: NewSessionDraft): boolean {
  return Boolean(
    draft.text.trim()
    || draft.attachments.length
    || draft.quotes.length
    || draft.goal_armed
    || draft.selected_skill_name
    || draft.selected_mcp,
  );
}

export function isDraftCustomized(draft: NewSessionDraft, default_model_key: ModelKey | null): boolean {
  return isDraftMeaningful(draft)
    || draft.model_key !== default_model_key
    || draft.reasoning_effort !== null
    || draft.variant !== "build"
    || draft.approval_mode !== "ask";
}

function createDraft(key: NewSessionDraftKey, default_model_key: ModelKey | null): NewSessionDraft {
  return {
    key,
    workspace_id: workspaceForDraftKey(key),
    text: "",
    attachments: [],
    quotes: [],
    model_key: default_model_key,
    reasoning_effort: null,
    variant: "build",
    approval_mode: "ask",
    goal_armed: false,
    selected_skill_name: null,
    selected_mcp: null,
    skill_options: [],
    skill_status: "idle",
    materialization_attempt: null,
  };
}
