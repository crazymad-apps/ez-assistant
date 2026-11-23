import { action, makeObservable, observable, observableRef, runInAction } from "mobx";
import type {
  AgentVariant, AttachmentId, McpSelectionTagSnapshot, MessageId, QuotedTextSnapshot,
  ReenterFromUserMessageRequest, ReenterFromUserMessageResult, RuntimeEventEnvelope, SessionId,
  SkillSummarySnapshot, UserMessageEditBlockedReason, UserMessageEditPreparation,
  UserMessageMcpEdit, UserMessageSkillEdit,
} from "@ez-assistant/protocol";
import { isCommandOutcomeUnknown, RuntimeClientError, type RuntimeClient } from "../runtime-client/RuntimeClient";
import type { ClientResources } from "../runtime-client/ClientResources";
import type { ComposerAttachment } from "./NewSessionDraftStore";
import type { RootStore } from "./RootStore";

type Ready = Extract<UserMessageEditPreparation, { type: "ready" }>;

type OrdinaryDraft = Readonly<{
  text: string;
  goal_armed: boolean;
  skill: SkillSummarySnapshot | null;
  mcp: McpSelectionTagSnapshot | null;
  attachments: readonly ComposerAttachment[];
  drawer: "goal" | "queue" | null;
}>;

const EMPTY_DRAFT: OrdinaryDraft = {
  text: "", goal_armed: false, skill: null, mcp: null, attachments: [], drawer: "queue",
};

type EditFields = Readonly<{
  text: string;
  variant: AgentVariant;
  attachments: readonly ComposerAttachment[];
  quotes: readonly QuotedTextSnapshot[];
  skill_edit: UserMessageSkillEdit;
  mcp_edit: UserMessageMcpEdit;
  mcp: McpSelectionTagSnapshot | null;
}>;

type Attempt = Readonly<{
  request: ReenterFromUserMessageRequest;
  client: RuntimeClient;
  auto_used: boolean;
}>;

export type HistoryEditDraft = EditFields & Readonly<{
  key: string;
  message_id: MessageId;
  initial: Ready;
  basis: Ready;
  observed_sequence: number;
  instance_id: string;
  source_client: RuntimeClient;
  phase: "editing" | "submitting" | "uncertain";
  attempt?: Attempt;
  error?: string;
  stale: boolean;
  locating: boolean;
  files: ClientResources;
}>;

const BLOCKED: Record<UserMessageEditBlockedReason, string> = {
  session_not_idle: "会话当前无法编辑历史消息。",
  session_archived: "归档会话不可编辑。",
  message_not_found: "原消息已不可用。",
  not_local_user_input: "这条消息不能编辑。",
  input_unavailable: "无法还原这条消息的完整输入。",
  goal_objective_boundary: "这条消息属于当前目标的历史边界，无法编辑。",
};

const STALE = "历史已更新，请重新核对后发送。";
const UNCERTAIN = "发送结果尚未确认。";
const PREPARE_TIMEOUT_MS = 30_000;
const SUBMIT_TIMEOUT_MS = 120_000;

type EditSlot = { scope: string; session_id: SessionId; device: string | null };
type Accepted = { result: ReenterFromUserMessageResult; busy: boolean; attempted: boolean };

/** 每个身份、设备及会话保留独立草稿。冻结的请求只随同一登录域重投，读取失败不重投写入。 */
export class HistoryEditStore {
  readonly #drafts = observable.map<string, HistoryEditDraft>(undefined, { deep: false });
  readonly #ordinary = observable.map<string, OrdinaryDraft>(undefined, { deep: false });
  readonly #ordinary_files = new Map<string, ClientResources>();
  readonly #accepted = observable.map<string, Accepted>(undefined, { deep: false });
  readonly #last_scope = new Map<string | null, string>();
  readonly #requests = new Map<string, AbortController>();
  readonly #commits = new Map<string, { instance_id: string; sequence: number }>();
  loading: { slot: string; message_id: MessageId } | null = null;
  switching: { slot: string; message_id: MessageId } | null = null;
  focus_revision = 0;

  constructor(private readonly root: RootStore) {
    makeObservable(this, {
      loading: observableRef, switching: observableRef, focus_revision: observable,
      updateOrdinary: action, update: action, addQuote: action,
      updateAttachments: action,
      cancel: action, dismissSwitch: action, selectionChanged: action, dispose: action,
    });
  }

  get ordinaryKey(): string {
    return this.#slotKey(this.#slot()) ?? "no-session";
  }

  ordinary(key: string): OrdinaryDraft {
    return this.#ordinary.get(key) ?? EMPTY_DRAFT;
  }

  updateOrdinary(key: string, change: Partial<OrdinaryDraft>): void {
    if (key === "no-session") return;
    if (change.attachments?.length && key === this.ordinaryKey) this.#ordinary_files.set(key, this.root.files);
    this.#ordinary.set(key, { ...this.ordinary(key), ...change });
  }

  selectionChanged(): void {
    for (const request of this.#requests.values()) request.abort();
    this.#requests.clear();
    this.loading = null;
    this.switching = null;
  }

  get draft(): HistoryEditDraft | undefined {
    return this.#drafts.get(this.ordinaryKey);
  }

  get accepted(): Accepted | undefined {
    return this.#accepted.get(this.ordinaryKey);
  }

  get busyTarget(): MessageId | null {
    return this.loading?.slot === this.ordinaryKey ? this.loading.message_id : null;
  }

  get switchTarget(): MessageId | null {
    return this.switching?.slot === this.ordinaryKey ? this.switching.message_id : null;
  }

  get stale(): boolean {
    const draft = this.draft;
    if (!draft) return false;
    const instance = this.root.connection.instance_id;
    const client = this.root.runtime_client;
    const view = this.#view();
    const generation = view?.conversation_generation;
    return draft.stale || (instance !== null && instance !== draft.instance_id)
      || (client !== null && client !== draft.source_client)
      || (generation !== undefined && generation > draft.basis.generation)
      || (generation === draft.basis.generation && (view?.session.message_count ?? 0) > draft.basis.message_count);
  }

  get sessionBusy(): boolean {
    const view = this.#view();
    if (!view) return true;
    const session = view.session;
    return Boolean(session.lifecycle !== "active" || this.root.live_execution.activeRunId(session.session_id, session.active_run_id)
      || session.queued_input_count || view.queue.items.length || session.active_child_count
      || session.active_compaction || view.approvals.items.length || this.root.feedback.waiting(session.session_id)
      || (view.goal && view.goal.state !== "paused")
      || this.root.pending_compaction_session_id === session.session_id || this.root.pending_session_action
      || this.root.composer_pending);
  }

  async begin(message_id: MessageId, discard = false): Promise<void> {
    const slot = this.#slot();
    const key = this.#slotKey(slot);
    const client = this.#connectedClient();
    if (!slot || !key || !client || this.root.navigation.selected_child_task_id || this.sessionBusy) return;
    const draft = this.#drafts.get(key);
    if (draft?.phase === "submitting" || draft?.phase === "uncertain" || this.busyTarget) return;
    if (draft?.message_id === message_id) {
      runInAction(() => this.focus_revision++);
      return;
    }
    if (draft && hasChanges(draft) && !discard) {
      runInAction(() => { this.switching = { slot: key, message_id }; });
      return;
    }
    const token = new AbortController();
    this.#requests.set(key, token);
    runInAction(() => { this.loading = { slot: key, message_id }; this.switching = null; });
    try {
      const { payload: { snapshot } } = await client.command({
        type: "get_user_message_edit", payload: { session_id: slot.session_id, message_id },
      }, { signal: AbortSignal.any([token.signal, AbortSignal.timeout(PREPARE_TIMEOUT_MS)]) });
      if (!this.#sameRead(key, client, token) || this.sessionBusy) return;
      if (snapshot.value.type === "blocked") {
        this.root.showInteractionError(BLOCKED[snapshot.value.reason]);
        return;
      }
      if (snapshot.value.message.message_id !== message_id) throw new Error("Runtime 返回了不匹配的消息。");
      const view = this.#view();
      if (!view) return;
      const basis = snapshot.value;
      const by_id = new Map((basis.message.attachments ?? []).map(item => [item.attachment_id, item]));
      const attachments = basis.message.attachment_ids.map((id): ComposerAttachment => {
        const item = by_id.get(id);
        const error = item?.state === "ready" ? null : "原附件不可用，请移除或替换。";
        return {
          selection_id: `history:${id}`, attachment_id: id, original_name: item?.original_name ?? "原附件",
          size_bytes: item?.size_bytes ?? 0, media_type: item?.media_type ?? null,
          origin: "file_picker", state: error ? "failed" : "uploaded", error, operation_id: null,
        };
      });
      if (draft) this.#release(draft);
      runInAction(() => {
        this.#drafts.set(key, {
          key: crypto.randomUUID(), message_id, basis, initial: basis,
          observed_sequence: snapshot.observed_sequence, instance_id: client.instance_id, source_client: client,
          text: basis.message.text, variant: basis.variant, attachments, quotes: basis.message.quotes,
          skill_edit: { type: "retain" }, mcp_edit: { type: "retain" },
          mcp: basis.message.mcp_selection ?? null, phase: "editing", locating: false,
          stale: this.#changedSince(key, client.instance_id, snapshot.observed_sequence),
          files: this.root.files,
        });
        this.focus_revision++;
      });
    } catch (error: unknown) {
      if (this.#sameRead(key, client, token)) this.root.showInteractionError(messageFor(error));
    } finally {
      if (this.#requests.get(key) === token) {
        this.#requests.delete(key);
        runInAction(() => { if (this.loading?.slot === key) this.loading = null; });
      }
    }
  }

  dismissSwitch(): void {
    this.switching = null;
  }

  update(key: string, draft_id: string, change: Partial<EditFields>): void {
    const draft = this.#drafts.get(key);
    if (draft?.key !== draft_id || draft.phase !== "editing") return;
    this.#drafts.set(key, { ...draft, ...change });
  }

  updateAttachments(key: string, draft_id: string, attachments: readonly ComposerAttachment[]): void {
    const draft = this.#drafts.get(key);
    if (draft?.key !== draft_id || draft.phase === "uncertain" || draft.attempt) return;
    this.#drafts.set(key, { ...draft, attachments });
  }

  addQuote(quote: QuotedTextSnapshot): boolean {
    const draft = this.draft;
    if (!draft) return false;
    if (draft.quotes.length < 16) this.update(this.ordinaryKey, draft.key, { quotes: [...draft.quotes, quote] });
    return true;
  }

  cancel(): void {
    const draft = this.draft;
    if (!draft || draft.phase !== "editing") return;
    this.#drafts.delete(this.ordinaryKey);
    this.#release(draft);
    this.focus_revision++;
  }

  async recheck(): Promise<void> {
    const slot = this.#slot();
    const draft = this.draft;
    const key = this.ordinaryKey;
    const client = this.#connectedClient();
    if (!slot || !draft || draft.phase !== "editing" || !client || this.sessionBusy || this.busyTarget) return;
    const token = new AbortController();
    this.#requests.set(key, token);
    runInAction(() => { this.loading = { slot: key, message_id: draft.message_id }; });
    try {
      const { payload: { snapshot } } = await client.command({
        type: "get_user_message_edit", payload: { session_id: slot.session_id, message_id: draft.message_id },
      }, { signal: AbortSignal.any([token.signal, AbortSignal.timeout(PREPARE_TIMEOUT_MS)]) });
      const current = this.#drafts.get(key);
      if (!this.#sameRead(key, client, token) || current?.key !== draft.key || current.phase !== "editing") return;
      const value = snapshot.value;
      if (value.type === "blocked") {
        runInAction(() => this.#drafts.set(key, { ...current, stale: true, error: BLOCKED[value.reason] }));
      } else if (value.message.message_id === draft.message_id) {
        runInAction(() => this.#drafts.set(key, {
          ...current, basis: value, observed_sequence: snapshot.observed_sequence, instance_id: client.instance_id, source_client: client,
          stale: this.#changedSince(key, client.instance_id, snapshot.observed_sequence), error: undefined,
        }));
      } else {
        throw new Error("Runtime 返回了不匹配的消息。");
      }
    } catch (error: unknown) {
      const current = this.#drafts.get(key);
      if (current?.key === draft.key && this.#sameRead(key, client, token)) {
        runInAction(() => this.#drafts.set(key, { ...current, error: messageFor(error) }));
      }
    } finally {
      if (this.#requests.get(key) === token) {
        this.#requests.delete(key);
        runInAction(() => { if (this.loading?.slot === key) this.loading = null; });
      }
    }
  }

  async locateOriginal(): Promise<void> {
    const slot = this.#slot();
    const draft = this.draft;
    const key = this.ordinaryKey;
    if (!slot || !draft || draft.locating) return;
    runInAction(() => this.#drafts.set(key, { ...draft, locating: true }));
    const found = await this.root.locateHistoryEditMessage(slot.session_id, draft.message_id);
    const current = this.#drafts.get(key);
    if (current?.key === draft.key) {
      runInAction(() => this.#drafts.set(key, { ...current, locating: false, error: found ? current.error : "无法定位原消息，请重试。" }));
    }
  }

  async submit(upload: () => Promise<readonly AttachmentId[] | null>): Promise<void> {
    const slot = this.#slot();
    const draft = this.draft;
    const key = this.ordinaryKey;
    const client = this.#connectedClient();
    if (!slot || !client || !draft || draft.phase !== "editing" || this.sessionBusy
      || this.stale || this.busyTarget
      || draft.attachments.some(item => item.attachment_id && item.state === "failed")
      || (!draft.text.trim() && !draft.attachments.length && !draft.quotes.length)) return;
    runInAction(() => this.#drafts.set(key, { ...draft, phase: "submitting", error: undefined }));
    let attachment_ids: readonly AttachmentId[] | null;
    try { attachment_ids = await upload(); } catch { attachment_ids = null; }
    const current = this.#drafts.get(key);
    if (current?.key !== draft.key) return;
    if (!attachment_ids || !this.#sameSelection(key, client) || this.sessionBusy || this.stale) {
      runInAction(() => this.#drafts.set(key, {
        ...current, phase: "editing",
        attachments: current.attachments.map(item => item.state === "uploading"
          ? { ...item, state: "selected", operation_id: null } : item),
      }));
      return;
    }
    const attempt: Attempt = { client, auto_used: false, request: {
      session_id: slot.session_id, message_id: draft.message_id,
      message: draft.text, variant: draft.variant, attachment_ids: [...attachment_ids],
      quotes: [...draft.quotes], skill_edit: draft.skill_edit, mcp_edit: draft.mcp_edit,
      expected_generation: draft.basis.generation, expected_message_count: draft.basis.message_count,
      idempotency_key: crypto.randomUUID(),
    } };
    await this.#send(key, current, attempt, client);
  }

  async retry(automatic = false): Promise<void> {
    const draft = this.draft;
    const client = this.#connectedClient();
    if (!client || draft?.phase !== "uncertain" || !draft.attempt) return;
    if (automatic && (draft.attempt.auto_used || draft.attempt.client === client)) return;
    const attempt = { ...draft.attempt, client, auto_used: automatic || draft.attempt.auto_used };
    await this.#send(this.ordinaryKey, draft, attempt, client);
  }

  async refresh(automatic = false): Promise<void> {
    const slot = this.#slot();
    const key = this.ordinaryKey;
    const accepted = this.#accepted.get(key);
    if (!slot || !accepted || accepted.busy || (automatic && accepted.attempted) || !this.#connectedClient()) return;
    runInAction(() => this.#accepted.set(key, { ...accepted, busy: true, attempted: true }));
    let ok = false;
    try {
      ok = await this.root.refreshHistoryEdit(slot.session_id, accepted.result);
    } catch {
      // 已接纳的写入与刷新失败分别处理。
    } finally {
      runInAction(() => {
        if (this.#accepted.get(key)?.result !== accepted.result) return;
        if (ok) this.#accepted.delete(key);
        else this.#accepted.set(key, { ...accepted, busy: false, attempted: true });
      });
    }
  }

  observe({ event, sequence }: RuntimeEventEnvelope, client: RuntimeClient | null): void {
    if (!client || (event.type !== "conversation_committed" && event.type !== "step_committed")
      || event.owner.type !== "main_session") return;
    const key = this.#slotKey({ scope: this.#scope(client), device: client.target_device, session_id: event.owner.session_id })!;
    if (this.#drafts.has(key) || this.#requests.has(key)) this.#commits.set(key, { instance_id: client.instance_id, sequence });
    const draft = this.#drafts.get(key);
    if (!draft || draft.phase === "uncertain" || draft.attempt || draft.instance_id !== client.instance_id
      || sequence <= draft.observed_sequence) return;
    runInAction(() => this.#drafts.set(key, { ...draft, stale: true }));
  }

  dispose(): void {
    for (const draft of this.#drafts.values()) this.#release(draft);
    for (const [key, draft] of this.#ordinary) {
      const files = this.#ordinary_files.get(key);
      if (!files) continue;
      for (const item of draft.attachments) {
        if (!item.attachment_id) void files.releaseAttachmentSelection(item.selection_id).catch(() => undefined);
      }
    }
    this.#drafts.clear();
    this.#ordinary.clear();
    this.#ordinary_files.clear();
    this.#accepted.clear();
    this.selectionChanged();
    this.#commits.clear();
  }

  async #send(key: string, draft: HistoryEditDraft, attempt: Attempt, client: RuntimeClient): Promise<void> {
    runInAction(() => this.#drafts.set(key, { ...draft, attempt, phase: "submitting", error: undefined }));
    try {
      // 超时只停止等待；Host 可能已提交。后续核实复用同一个冻结请求。
      const { payload } = await client.command({ type: "reenter_from_user_message", payload: attempt.request },
        { signal: AbortSignal.timeout(SUBMIT_TIMEOUT_MS) });
      if (this.#drafts.get(key)?.attempt !== attempt) return;
      runInAction(() => {
        this.#drafts.delete(key);
        this.#commits.delete(key);
        this.#accepted.set(key, { result: payload, busy: false, attempted: false });
        if (this.ordinaryKey === key) this.focus_revision++;
      });
      if (this.ordinaryKey === key) await this.refresh(true);
    } catch (error: unknown) {
      const current = this.#drafts.get(key);
      if (current?.attempt !== attempt) return;
      const uncertain = isCommandOutcomeUnknown(error);
      const stale = error instanceof RuntimeClientError && ["snapshot_stale", "user_message_not_editable"].includes(error.code);
      let message = messageFor(error);
      if (uncertain) message = UNCERTAIN;
      else if (stale) message = draft.phase === "uncertain"
        ? "当前历史已更新，无法确认原发送结果，请重新核对。" : STALE;
      runInAction(() => this.#drafts.set(key, {
        ...current, phase: uncertain ? "uncertain" : "editing",
        attempt: uncertain ? attempt : undefined, stale: current.stale || stale,
        error: message,
      }));
    }
  }

  #scope(client: RuntimeClient): string {
    const local = this.root.local_runtime_client;
    return JSON.stringify([local?.address, local?.login_context, client.address, client.login_context]);
  }

  #slot(): EditSlot | null {
    const session_id = this.root.navigation.selected_session_id;
    const device = this.root.navigation.target_device;
    const client = this.root.runtime_client;
    if (client) this.#last_scope.set(device, this.#scope(client));
    if (!session_id) return null;
    return { session_id, device, scope: this.#last_scope.get(device) ?? "" };
  }

  #slotKey(slot: EditSlot | null): string | null {
    return slot ? JSON.stringify([slot.scope, slot.device, slot.session_id]) : null;
  }

  #connectedClient(): RuntimeClient | null {
    if (this.root.connection.state !== "connected" || this.root.projection.is_stale
      || !this.root.projection.application?.capabilities.user_message_edit) return null;
    return this.root.runtime_client;
  }

  #sameSelection(key: string, client: RuntimeClient): boolean {
    return key === this.ordinaryKey && !this.root.navigation.selected_child_task_id && client === this.#connectedClient();
  }

  #sameRead(key: string, client: RuntimeClient, token: AbortController): boolean {
    return this.#requests.get(key) === token && this.#sameSelection(key, client);
  }

  #view() {
    const session_id = this.root.navigation.selected_session_id;
    return session_id ? this.root.projection.session_views.get(session_id) : undefined;
  }

  #changedSince(key: string, instance_id: string, sequence: number): boolean {
    const latest = this.#commits.get(key);
    return latest?.instance_id === instance_id && latest.sequence > sequence;
  }

  #release(draft: HistoryEditDraft): void {
    for (const attachment of draft.attachments) {
      if (!attachment.attachment_id) void draft.files.releaseAttachmentSelection(attachment.selection_id).catch(() => undefined);
    }
  }
}

function hasChanges(draft: HistoryEditDraft): boolean {
  const original = draft.initial;
  return draft.text !== original.message.text || draft.variant !== original.variant
    || draft.attachments.length !== original.message.attachment_ids.length
    || draft.attachments.some((item, index) => item.attachment_id !== original.message.attachment_ids[index])
    || draft.quotes.length !== original.message.quotes.length
    || draft.quotes.some((item, index) => item.quote_id !== original.message.quotes[index]?.quote_id)
    || draft.skill_edit.type === "select" || (draft.skill_edit.type === "clear" && !!original.message.skill)
    || draft.mcp_edit.type === "select" || (draft.mcp_edit.type === "clear" && !!original.message.mcp_selection);
}

function messageFor(error: unknown): string {
  if (error instanceof DOMException && error.name === "TimeoutError") return "读取原消息超时，请重试。";
  if (error instanceof RuntimeClientError) {
    if (["session_not_idle", "session_busy", "session_compaction_in_progress"].includes(error.code)) return BLOCKED.session_not_idle;
    if (error.code === "session_archived") return BLOCKED.session_archived;
    if (error.code === "invalid_request") return "当前修改无法提交，请检查输入后重试。";
  }
  return error instanceof Error ? error.message : "无法完成消息编辑，请重试。";
}
