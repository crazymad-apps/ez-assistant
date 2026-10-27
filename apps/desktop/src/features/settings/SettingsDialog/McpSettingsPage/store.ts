import { priorityLabel } from "./draft";
import { action, makeObservable, observable, observableRef, runInAction } from "mobx";
import type { McpConfigurationMutation, McpConfigurationSnapshot, McpServerDraft, McpManagedServerTestDraft, McpServerSnapshot, PreviewMcpImportResult, TestMcpServerResult } from "@ez-assistant/protocol";
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
  pending_action: "save" | "preview" | "refresh" | null = null;
  error_message: string | null = null;
  notice_message: string | null = null;
  configuration_conflict = false;

  constructor(private readonly getClient: () => RuntimeClient | null) {
    makeObservable(this, {
      configuration: observableRef, loading: observable, stale: observable,
      testing: observable, test_result: observableRef, pending_action: observable,
      error_message: observable, notice_message: observable, configuration_conflict: observable,
      load: action, mutate: action, previewImport: action, test: action, refresh: action, saveParameter: action,
      cancelTest: action, deactivate: action, clearMessages: action, dismissNotice: action,
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

  async saveParameter(server: McpServerSnapshot, name: string, value: string | null): Promise<McpServerSnapshot | null> {
    const managed = server.managed;
    if (server.source.type !== "plugin" || !managed?.plugin_sha256 || !managed.plugin_settings_revision || this.stale) return null;
    const source = server.source;
    const result = await this.runAction("save", async client => {
      await client.command({ type: "mutate_plugin_settings", payload: { name: source.name, expected_active_sha256: managed.plugin_sha256!, expected_revision: managed.plugin_settings_revision!, mutation: { type: "set_parameter", name, value } } });
      return (await client.command({ type: "get_mcp_configuration", payload: {} })).payload.snapshot;
    }, snapshot => { ++this.#request; this.loading = false; this.configuration = snapshot; this.notice_message = "插件参数已保存，共用此参数的服务也会受到影响。"; });
    return result?.servers.find(item => item.source.type === "plugin" && item.source.name === source.name && item.server_key === server.server_key) ?? null;
  }

  async refresh(server: McpServerSnapshot, revision: string): Promise<McpServerSnapshot | null> {
    if (this.stale) return null;
    const result = await this.runAction("refresh", async client => (await client.command({ type: "refresh_mcp_server", payload: { target: { source: server.source, server_key: server.server_key }, expected_revision: server.managed?.revision ?? revision, expected_definition_digest: server.managed?.definition_digest ?? "" } })).payload, result => {
      this.notice_message = result.servers.map(item => `${item.server_key}：${refreshLabel(item.outcome)}`).join("；");
    });
    if (!result) return null;
    await this.load();
    if (this.stale) return null;
    const refreshed = this.configuration?.servers.find(item => JSON.stringify(item.source) === JSON.stringify(server.source) && item.server_key === server.server_key) ?? null;
    if (result.outcome === "success" && refreshed && priorityLabel(refreshed)) runInAction(() => { this.notice_message = `已按优先级刷新。${priorityLabel(refreshed)}`; });
    return refreshed;
  }

  previewImport(document: string): Promise<PreviewMcpImportResult | null> {
    return this.runAction("preview", async client => {
      return (await client.command({ type: "preview_mcp_import", payload: { document } })).payload;
    });
  }

  async test(server: McpServerDraft | McpManagedServerTestDraft): Promise<void> {
    const client = this.getClient();
    if (!client || this.testing) return;
    const owner = this.#owner;
    const test = { id: crypto.randomUUID(), client };
    this.#test = test;
    this.testing = true;
    this.test_result = null;
    this.clearMessages();
    try {
      const result = await client.command({ type: "test_mcp_server", payload: { test_id: test.id, ...("target" in server ? { managed: server } : { server }) } });
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

  dismissNotice(message: string): void {
    if (this.notice_message === message) this.notice_message = null;
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

  private async runAction<T>(name: "save" | "preview" | "refresh", operation: (client: RuntimeClient) => Promise<T>, commit?: (result: T) => void): Promise<T | null> {
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
          && "code" in error && (error.code === "mcp_config_conflict" || error.code === "plugin_revision_conflict" || error.code === "plugin_active_changed");
      });
      return null;
    } finally {
      if (this.#action === action_owner) runInAction(() => { this.#action = null; this.pending_action = null; });
    }
  }
}

function refreshLabel(outcome: string): string {
  const labels: Record<string, string> = { refreshed: "已刷新", connected_without_tools: "已连接，无工具", disabled: "已关闭连接", removed: "已移除连接", blocked_by_conflict: "同名冲突，未连接", retained_after_failure: "刷新失败，保留原连接", deferred: "等待连接", not_found: "服务已不存在" };
  return labels[outcome] ?? "请查看服务诊断";
}
