import { action, makeObservable, observable, observableRef, runInAction } from "mobx";
import type {
  MemoryCapabilities,
  PersonaSnapshot,
  PinnedMemoryCollectionSnapshot,
  PinnedMemorySnapshot,
} from "../generated/assistant-protocol";
import type { RuntimeClient } from "../runtime-client/RuntimeClient";

type MemorySettingsDependencies = Readonly<{
  get_client: () => RuntimeClient | null;
}>;

export class MemorySettingsStore {
  loading = false;
  pending_action: string | null = null;
  persona: PersonaSnapshot | null = null;
  collection: PinnedMemoryCollectionSnapshot | null = null;
  capabilities: MemoryCapabilities | null = null;
  error_message: string | null = null;
  notice_message: string | null = null;
  conflict_message: string | null = null;

  constructor(private readonly dependencies: MemorySettingsDependencies) {
    makeObservable(this, {
      loading: observable,
      pending_action: observable,
      persona: observableRef,
      collection: observableRef,
      capabilities: observableRef,
      error_message: observable,
      notice_message: observable,
      conflict_message: observable,
      load: action,
      savePersona: action,
      createPinnedMemory: action,
      updatePinnedMemory: action,
      deletePinnedMemory: action,
      clearMessages: action,
      showError: action,
    });
  }

  clearMessages(): void {
    this.error_message = null;
    this.notice_message = null;
    this.conflict_message = null;
  }

  showError(message: string): void {
    this.error_message = message;
    this.notice_message = null;
    this.conflict_message = null;
  }

  async load(): Promise<void> {
    const client = this.requireClient();
    if (!client) return;
    this.loading = true;
    this.clearMessages();
    try {
      const [persona_result, pinned_result] = await Promise.all([
        client.command({ type: "get_persona", payload: {} }),
        client.command({ type: "list_pinned_memories", payload: {} }),
      ]);
      runInAction(() => {
        this.persona = persona_result.payload.persona;
        this.capabilities = persona_result.payload.capabilities;
        this.collection = pinned_result.payload.collection;
      });
    } catch (error: unknown) {
      runInAction(() => {
        this.error_message = displayError(error);
      });
    } finally {
      runInAction(() => {
        this.loading = false;
      });
    }
  }

  async savePersona(enabled: boolean, content: string): Promise<boolean> {
    const client = this.requireClient();
    if (!client || !this.persona) return false;
    return this.runAction("persona:save", async () => {
      const result = await client.command({
        type: "set_persona",
        payload: {
          enabled,
          content,
          expected_revision: this.persona?.revision ?? 0,
        },
      });
      this.persona = result.payload.persona;
      if (!result.payload.applied) {
        this.conflict_message = "Persona 已在其他位置更新。你的草稿仍保留，请核对最新内容后再次保存。";
        return;
      }
      this.notice_message = "Persona 已保存，只会影响之后新建的会话。";
    });
  }

  async createPinnedMemory(category: string, content: string): Promise<boolean> {
    const client = this.requireClient();
    if (!client || !this.collection) return false;
    return this.runAction("pinned:create", async () => {
      const result = await client.command({
        type: "create_pinned_memory",
        payload: {
          expected_collection_revision: this.collection?.revision ?? 0,
          category,
          content,
          attributes: {},
        },
      });
      this.collection = result.payload.collection;
      if (!result.payload.applied) {
        this.conflict_message = "Pinned Memory 列表已更新。你的草稿仍保留，请核对后再次保存。";
        return;
      }
      this.notice_message = "Pinned Memory 已添加。";
    });
  }

  async updatePinnedMemory(memory: PinnedMemorySnapshot, category: string, content: string): Promise<boolean> {
    const client = this.requireClient();
    if (!client) return false;
    return this.runAction(`pinned:update:${memory.id}`, async () => {
      const result = await client.command({
        type: "update_pinned_memory",
        payload: {
          id: memory.id,
          expected_revision: memory.revision,
          category,
          content,
          attributes: memory.attributes,
        },
      });
      this.collection = result.payload.collection;
      if (!result.payload.applied) {
        this.conflict_message = "该条 Pinned Memory 已被更新。你的草稿仍保留，请核对后再次保存。";
        return;
      }
      this.notice_message = "Pinned Memory 已保存。";
    });
  }

  async deletePinnedMemory(memory: PinnedMemorySnapshot): Promise<boolean> {
    const client = this.requireClient();
    if (!client) return false;
    return this.runAction(`pinned:delete:${memory.id}`, async () => {
      const result = await client.command({
        type: "delete_pinned_memory",
        payload: { id: memory.id, expected_revision: memory.revision },
      });
      this.collection = result.payload.collection;
      if (!result.payload.applied) {
        this.conflict_message = "该条 Pinned Memory 已发生变化，列表已重新加载，请再次确认。";
        return;
      }
      this.notice_message = "Pinned Memory 已删除。";
    });
  }

  private requireClient(): RuntimeClient | null {
    const client = this.dependencies.get_client();
    if (!client) this.error_message = "Runtime 尚未连接。";
    return client;
  }

  private async runAction(name: string, operation: () => Promise<void>): Promise<boolean> {
    this.pending_action = name;
    this.clearMessages();
    try {
      await operation();
      return this.conflict_message === null;
    } catch (error: unknown) {
      runInAction(() => {
        this.error_message = displayError(error);
      });
      return false;
    } finally {
      runInAction(() => {
        this.pending_action = null;
      });
    }
  }
}

function displayError(error: unknown): string {
  return error instanceof Error ? error.message : "操作失败，请重试。";
}
