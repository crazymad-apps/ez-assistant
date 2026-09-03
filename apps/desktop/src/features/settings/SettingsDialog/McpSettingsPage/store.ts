import { action, makeObservable, observable, observableRef, runInAction } from "mobx";
import type { McpConfigurationMutation, McpConfigurationSnapshot, McpServerDraft, PreviewMcpImportResult, TestMcpServerResult } from "../../../../generated/assistant-protocol";
import type { RuntimeClient } from "../../../../runtime-client/RuntimeClient";

/** MCP 设置的请求与反馈归属；不保存工具目录、连接或权限的权威状态。 */
export class McpSettingsStore {
  #owner = {};
  #request = 0;
  #action: object | null = null;
  #test: { id: string; client: RuntimeClient } | null = null;
  configuration: McpConfigurationSnapshot | null = null;
  loading = false;
  stale = false;
  testing = false;
  test_result: TestMcpServerResult | null = null;
  pending_action: "save" | "preview" | null = null;
  error_message: string | null = null;
  notice_message: string | null = null;
  configuration_conflict = false;

  constructor(private readonly getClient: () => RuntimeClient | null) {
    makeObservable(this, {
      configuration: observableRef, loading: observable, stale: observable,
      testing: observable, test_result: observableRef, pending_action: observable,
      error_message: observable, notice_message: observable, configuration_conflict: observable,
      load: action, mutate: action, previewImport: action, test: action,
      cancelTest: action, deactivate: action, clearMessages: action,
    });
  }

  async load(): Promise<void> {
    const client = this.getClient();
    if (!client) { this.stale = true; return; }
    const owner = this.#owner;
    const request = ++this.#request;
    this.loading = true;
    try {
      const result = await client.command({ type: "get_mcp_configuration", payload: {} });
      if (!this.current(owner, client) || request !== this.#request) return;
      runInAction(() => { this.configuration = result.payload.snapshot; this.stale = false; });
    } catch {
      if (this.current(owner, client) && request === this.#request) runInAction(() => {
        this.stale = true;
        this.error_message = "MCP 配置读取失败，当前列表可能已过期。请重新读取。";
      });
    } finally {
      if (owner === this.#owner && request === this.#request) runInAction(() => {
        this.loading = false;
        if (client !== this.getClient()) { this.configuration = null; this.stale = true; }
      });
    }
  }

  async mutate(expected_revision: string, mutation: McpConfigurationMutation): Promise<boolean> {
    if (this.stale) return false;
    const result = await this.runAction("save", async client => {
      return (await client.command({ type: "mutate_mcp_configuration", payload: { expected_revision, mutation } })).payload;
    }, result => {
      ++this.#request;
      this.configuration = result.snapshot;
      this.loading = false;
      this.notice_message = result.snapshot.needs_refresh ? null : "配置已保存。";
    });
    return result !== null;
  }

  previewImport(document: string): Promise<PreviewMcpImportResult | null> {
    return this.runAction("preview", async client => {
      return (await client.command({ type: "preview_mcp_import", payload: { document } })).payload;
    });
  }

  async test(server: McpServerDraft): Promise<void> {
    const client = this.getClient();
    if (!client || this.testing) return;
    const owner = this.#owner;
    const test = { id: crypto.randomUUID(), client };
    this.#test = test;
    this.testing = true;
    this.test_result = null;
    this.clearMessages();
    try {
      const result = await client.command({ type: "test_mcp_server", payload: { test_id: test.id, server } });
      if (this.current(owner, client) && this.#test === test) runInAction(() => { this.test_result = result.payload; });
    } catch {
      if (this.current(owner, client) && this.#test === test) runInAction(() => {
        this.error_message = "MCP 连接测试失败，请检查草稿配置和连接状态。";
      });
    } finally {
      if (this.#test === test) runInAction(() => { this.#test = null; this.testing = false; });
    }
  }

  cancelTest(): void {
    const test = this.#test;
    this.#test = null;
    this.testing = false;
    this.test_result = null;
    // 取消发给创建测试的客户端，不能因 Runtime 重连误取消新实例中的同名请求。
    if (test) void test.client.command({ type: "cancel_mcp_server_test", payload: { test_id: test.id } }).catch(() => undefined);
  }

  clearMessages(): void {
    this.error_message = null;
    this.notice_message = null;
    this.configuration_conflict = false;
  }

  deactivate(): void {
    this.cancelTest();
    this.#owner = {};
    ++this.#request;
    this.#action = null;
    this.loading = false;
    this.pending_action = null;
    this.stale = true;
    this.clearMessages();
  }

  dispose(): void { this.deactivate(); }

  private current(owner: object, client: RuntimeClient): boolean {
    return owner === this.#owner && client === this.getClient();
  }

  private async runAction<T>(name: "save" | "preview", operation: (client: RuntimeClient) => Promise<T>, commit?: (result: T) => void): Promise<T | null> {
    const client = this.getClient();
    if (!client || this.pending_action) return null;
    const owner = this.#owner;
    const action_owner = {};
    this.#action = action_owner;
    this.pending_action = name;
    this.clearMessages();
    try {
      const result = await operation(client);
      if (!this.current(owner, client) || this.#action !== action_owner) return null;
      runInAction(() => commit?.(result));
      return result;
    } catch (error: unknown) {
      if (this.current(owner, client) && this.#action === action_owner) runInAction(() => {
        this.error_message = "MCP 操作失败，请检查配置或重新读取后重试。";
        this.configuration_conflict = typeof error === "object" && error !== null
          && "code" in error && error.code === "mcp_config_conflict";
      });
      return null;
    } finally {
      if (this.#action === action_owner) runInAction(() => { this.#action = null; this.pending_action = null; });
    }
  }
}
