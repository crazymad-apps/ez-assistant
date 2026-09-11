import { McpSettingsStore } from "../features/settings";
import { action, makeObservable, observable, observableRef, runInAction } from "mobx";
import type {
  ConfigurationStatus,
  ConnectionValidationFailure,
  ModelConfigOrigin,
  ModelSelection,
  ModelSettings,
  ProviderSummary,
  ProviderUsage,
  ProviderInstanceId,
  DiscoveredModel,
  ProviderConnection,
  ProviderCredentialChange,
  ModelConfigurationDetail,
  ModelFixedConfig,
  ModelParameters,
  PermissionDocumentDraft,
  PermissionDocumentRevision,
  PermissionDocumentScope,
  PermissionDocumentSnapshot,
  SessionId,
  SkillDetailSnapshot,
  SkillManagementSnapshot,
  ValidateModelConnectionResult,
  WorkspaceId,
} from "@ez-assistant/protocol";
import type { RuntimeClient } from "../runtime-client/RuntimeClient";

export type RuntimeSettingsPageId = "runtime" | "runtime_connection" | "host_access" | "runtime_diagnostics" | "runtime_local";
export type SettingsPage = RuntimeSettingsPageId | "models" | "permissions" | "memory" | "skills" | "devices" | "mcp";

type SettingsDependencies = Readonly<{
  get_client: () => RuntimeClient | null;
  get_permission_context: () => Readonly<{
    session_id: SessionId | null;
    workspace_id: WorkspaceId | null;
  }>;
  refresh_application: () => Promise<void>;
}>;

export class SettingsStore {
  #skill_scope_initialized_for_open = false;
  #skill_detail_request = 0;
  #skill_request = 0;
  // 仅标识前端读取；新读取或已提交 mutation 使先前响应失效，不属于持久化版本。
  #model_read = 0;
  readonly mcp: McpSettingsStore;
  is_open = false;
  page: SettingsPage = "runtime";
  loading = false;
  pending_action: string | null = null;
  status: ConfigurationStatus | null = null;
  providers: readonly ProviderSummary[] = [];
  model_settings: ModelSettings = { default_model: null, vision_model: null };
  error_message: string | null = null;
  notice_message: string | null = null;
  configuration_conflict = false;
  permission_documents: readonly PermissionDocumentSnapshot[] = [];
  permission_conflict = false;
  skill_workspace_id: WorkspaceId | null = null;
  skill_management: SkillManagementSnapshot | null = null;
  skill_detail: SkillDetailSnapshot | null = null;
  skill_detail_loading = false;
  skills_loading = false;
  pending_skill_name: string | null = null;

  constructor(private readonly dependencies: SettingsDependencies) {
    this.mcp = new McpSettingsStore(dependencies.get_client);
    makeObservable(this, {
      is_open: observable,
      page: observable,
      loading: observable,
      pending_action: observable,
      status: observableRef,
      providers: observableRef,
      model_settings: observableRef,
      error_message: observable,
      notice_message: observable,
      configuration_conflict: observable,
      permission_documents: observableRef,
      permission_conflict: observable,
      skill_workspace_id: observable,
      skill_management: observableRef,
      skill_detail: observableRef,
      skill_detail_loading: observable,
      skills_loading: observable,
      pending_skill_name: observable,
      open: action,
      close: action,
      selectPage: action,
      load: action,
      reloadConfiguration: action,
      loadPermissions: action,
      reloadPermissions: action,
      loadSkills: action,
      loadSkillDetail: action,
      clearSkillDetail: action,
      selectSkillWorkspace: action,
      setSkillEnabled: action,
      replacePermissionDocument: action,
      setDefaultModel: action,
      setAuxiliaryVisionModel: action,
      clearMessages: action,
      showError: action,
      showNotice: action,
    });
  }

  open(page: SettingsPage = "runtime"): void {
    this.is_open = true;
    this.page = page;
    this.#skill_scope_initialized_for_open = false;
    this.clearSkillDetail();
    void this.load();
    if (page === "permissions") void this.loadPermissions();
    if (page === "skills") {
      this.skill_workspace_id = this.dependencies.get_permission_context().workspace_id;
      this.#skill_scope_initialized_for_open = true;
      void this.loadSkills();
    }
  }

  close(): void {
    this.mcp.deactivate();
    this.is_open = false;
    this.error_message = null;
    this.notice_message = null;
    this.configuration_conflict = false;
    this.permission_conflict = false;
    this.#skill_scope_initialized_for_open = false;
    this.clearSkillDetail();
  }

  selectPage(page: SettingsPage): void {
    if (page !== "mcp") this.mcp.deactivate();
    this.page = page;
    this.clearMessages();
    if (page !== "skills") this.clearSkillDetail();
    if (page === "permissions") void this.loadPermissions();
    if (page === "skills") {
      if (!this.#skill_scope_initialized_for_open) {
        this.skill_workspace_id = this.dependencies.get_permission_context().workspace_id;
        this.#skill_scope_initialized_for_open = true;
      }
      void this.loadSkills();
    }
  }

  selectSkillWorkspace(workspace_id: WorkspaceId | null): void {
    if (this.skill_workspace_id === workspace_id) return;
    this.skill_workspace_id = workspace_id;
    void this.loadSkills();
  }

  async loadSkills(): Promise<void> {
    const client = this.requireClient();
    if (!client) return;
    this.clearSkillDetail();
    const workspace_id = this.skill_workspace_id;
    const request = ++this.#skill_request;
    this.skills_loading = true;
    this.clearMessages();
    try {
      const result = await client.command({
        type: "list_skills",
        payload: { workspace_id: workspace_id ?? undefined },
      });
      runInAction(() => {
        if (this.skill_workspace_id === workspace_id && this.#skill_request === request) {
          this.skill_management = result.payload.snapshot;
        }
      });
    } catch (error: unknown) {
      runInAction(() => {
        if (this.skill_workspace_id === workspace_id && this.#skill_request === request) {
          this.error_message = displayError(error);
        }
      });
    } finally {
      runInAction(() => {
        if (this.#skill_request === request) this.skills_loading = false;
      });
    }
  }

  async loadSkillDetail(name: string): Promise<void> {
    const client = this.requireClient();
    if (!client) return;
    const workspace_id = this.skill_workspace_id;
    const request = ++this.#skill_detail_request;
    this.skill_detail = null;
    this.skill_detail_loading = true;
    this.clearMessages();
    try {
      const result = await client.command({
        type: "get_skill_detail",
        payload: { workspace_id: workspace_id ?? undefined, name },
      });
      runInAction(() => {
        if (this.skill_workspace_id !== workspace_id || this.#skill_detail_request !== request) return;
        this.skill_detail = result.payload.detail ?? null;
        if (!result.payload.detail) {
          this.error_message = "技能已发生变化，请返回列表重新选择。";
        }
      });
    } catch (error: unknown) {
      runInAction(() => {
        if (this.skill_workspace_id === workspace_id && this.#skill_detail_request === request) {
          this.error_message = displayError(error);
        }
      });
    } finally {
      runInAction(() => {
        if (this.#skill_detail_request === request) this.skill_detail_loading = false;
      });
    }
  }

  clearSkillDetail(): void {
    this.#skill_detail_request += 1;
    this.skill_detail = null;
    this.skill_detail_loading = false;
  }

  async setSkillEnabled(name: string, enabled: boolean): Promise<boolean> {
    const client = this.requireClient();
    if (!client || this.pending_skill_name) return false;
    const workspace_id = this.skill_workspace_id;
    this.pending_skill_name = name;
    this.clearMessages();
    try {
      const result = await client.command({
        type: "set_skill_enabled",
        payload: { workspace_id: workspace_id ?? undefined, name, enabled },
      });
      runInAction(() => {
        if (this.skill_workspace_id === workspace_id) {
          this.skill_management = result.payload.snapshot;
          this.notice_message = enabled ? `已启用技能“${name}”。` : `已禁用技能“${name}”。`;
        }
      });
      return true;
    } catch (error: unknown) {
      runInAction(() => {
        this.error_message = displayError(error);
      });
      return false;
    } finally {
      runInAction(() => {
        this.pending_skill_name = null;
      });
    }
  }

  clearMessages(): void {
    this.error_message = null;
    this.notice_message = null;
    this.configuration_conflict = false;
    this.permission_conflict = false;
  }

  showError(message: string): void {
    this.error_message = message;
    this.notice_message = null;
  }

  showNotice(message: string): void {
    this.notice_message = message;
    this.error_message = null;
  }

  async load(): Promise<void> {
    const client = this.dependencies.get_client();
    if (!client) {
      this.error_message = "运行时尚未连接。";
      return;
    }
    const request = ++this.#model_read;
    this.loading = true;
    this.error_message = null;
    try {
      const [status, providers, settings] = await Promise.all([
        client.command({ type: "get_config_status", payload: {} }),
        client.command({ type: "list_providers", payload: {} }),
        client.command({ type: "get_model_settings", payload: {} }),
      ]);
      runInAction(() => {
        if (client !== this.dependencies.get_client() || request !== this.#model_read) return;
        this.status = status.payload.status;
        this.providers = providers.payload;
        this.model_settings = settings.payload;
        this.configuration_conflict = false;
      });
    } catch (error: unknown) {
      runInAction(() => {
        if (client !== this.dependencies.get_client() || request !== this.#model_read) return;
        this.error_message = displayError(error);
      });
    } finally {
      runInAction(() => {
        if (request === this.#model_read) this.loading = false;
      });
    }
  }

  async reloadConfiguration(): Promise<void> {
    const client = this.requireClient();
    if (!client) return;
    await this.runAction("reload", async () => {
      const result = await client.command({ type: "reload_config", payload: {} });
      this.status = result.payload.status;
      await this.loadModelSettings();
      this.notice_message = "配置已重新加载。";
      await this.dependencies.refresh_application();
    });
  }

  async loadPermissions(): Promise<void> {
    const client = this.requireClient();
    if (!client) return;
    const context = this.dependencies.get_permission_context();
    const scopes: PermissionDocumentScope[] = [{ type: "global" }];
    if (context.workspace_id) {
      scopes.unshift({ type: "workspace", payload: { workspace_id: context.workspace_id } });
    }
    if (context.session_id) {
      scopes.unshift({ type: "session", payload: { session_id: context.session_id } });
    }
    this.loading = true;
    this.clearMessages();
    try {
      const results = await Promise.all(scopes.map((scope) => client.command({
        type: "get_permission_document",
        payload: { scope },
      })));
      runInAction(() => {
        this.permission_documents = results.map((result) => result.payload.document);
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

  async reloadPermissions(): Promise<boolean> {
    const client = this.requireClient();
    const { session_id } = this.dependencies.get_permission_context();
    if (!client || !session_id) {
      this.error_message = "请先选择一个会话，再重新加载权限。";
      return false;
    }
    return this.runAction("permission:reload", async () => {
      const result = await client.command({
        type: "reload_permissions",
        payload: { session_id },
      });
      if (!result.payload.applied) {
        throw new Error(result.payload.diagnostics[0]?.message ?? "权限文件校验失败。" );
      }
      await this.loadPermissions();
      this.notice_message = "权限规则已重新加载。";
    });
  }

  async replacePermissionDocument(
    scope: PermissionDocumentScope,
    expected_revision: PermissionDocumentRevision,
    document: PermissionDocumentDraft,
  ): Promise<boolean> {
    const client = this.requireClient();
    if (!client) return false;
    return this.runAction("permission:save", async () => {
      const result = await client.command({
        type: "replace_permission_document",
        payload: { scope, expected_revision, document },
      });
      this.permission_documents = this.permission_documents.map((current) => (
        samePermissionScope(current.scope, scope) ? result.payload.document : current
      ));
      this.notice_message = "权限规则已保存。";
    });
  }

  async loadModelSettings(signal?: AbortSignal): Promise<boolean> {
    const client = this.requireClient();
    if (!client) return false;
    const request = ++this.#model_read;
    runInAction(() => { this.loading = true; });
    try {
      const [providers, settings] = await Promise.all([
        client.command({ type: "list_providers", payload: {} }, { signal }),
        client.command({ type: "get_model_settings", payload: {} }, { signal }),
      ]);
      if (signal?.aborted || client !== this.dependencies.get_client() || request !== this.#model_read) return false;
      runInAction(() => {
        this.providers = providers.payload;
        this.model_settings = settings.payload;
      });
      return true;
    } catch (error: unknown) {
      if (!signal?.aborted && client === this.dependencies.get_client() && request === this.#model_read) runInAction(() => this.showError(displayError(error)));
      return false;
    } finally {
      runInAction(() => { if (request === this.#model_read) this.loading = false; });
    }
  }

  async listProviderModels(provider_instance_id: ProviderInstanceId, signal?: AbortSignal): Promise<DiscoveredModel[]> {
    const client = this.requireClient();
    if (!client) throw new Error("运行时尚未连接。");
    const result = await client.command({ type: "list_provider_models", payload: { provider_instance_id } }, { signal });
    if (client !== this.dependencies.get_client()) throw new Error("运行时连接已切换，请重试。");
    return result.payload;
  }

  async setDefaultModel(selection: ModelSelection | null): Promise<boolean> {
    const client = this.requireClient();
    if (!client) return false;
    return this.runAction("default", async () => {
      const result = await client.command({ type: "set_default_model", payload: { selection } });
      if (client !== this.dependencies.get_client()) throw new Error("运行时连接已切换，请重新读取设置。");
      this.invalidateModelRead();
      runInAction(() => {
        this.model_settings = result.payload;
        this.notice_message = "默认模型已更新。";
      });
      await this.refreshAfterModelMutation(client);
    });
  }

  async setAuxiliaryVisionModel(selection: ModelSelection | null): Promise<boolean> {
    const client = this.requireClient();
    if (!client) return false;
    return this.runAction("vision-model", async () => {
      const result = await client.command({ type: "set_auxiliary_vision_model", payload: { selection } });
      if (client !== this.dependencies.get_client()) throw new Error("运行时连接已切换，请重新读取设置。");
      this.invalidateModelRead();
      runInAction(() => {
        this.model_settings = result.payload;
        this.notice_message = selection ? "默认识图模型已更新。" : "默认识图模型已清除。";
      });
      await this.refreshAfterModelMutation(client);
    });
  }

  async saveProvider(provider_instance_id: ProviderInstanceId | null, connection: ProviderConnection, credential: ProviderCredentialChange): Promise<ProviderSummary | null> {
    const client = this.requireClient();
    if (!client) return null;
    let saved: ProviderSummary | null = null;
    const applied = await this.runAction("provider:save", async () => {
      const result = provider_instance_id
        ? await client.command({ type: "update_provider", payload: { provider_instance_id, connection, credential } })
        : await client.command({ type: "create_provider", payload: { connection, credential } });
      if (client !== this.dependencies.get_client()) throw new Error("运行时连接已切换，请重新读取设置。");
      saved = result.payload;
      this.invalidateModelRead();
      runInAction(() => {
        this.providers = [...this.providers.filter((item) => item.provider_instance_id !== result.payload.provider_instance_id), result.payload];
        this.showNotice("服务商已保存。");
      });
      await this.refreshAfterModelMutation(client);
    });
    return applied ? saved : null;
  }

  async getProviderUsage(provider_instance_id: ProviderInstanceId, signal?: AbortSignal): Promise<ProviderUsage> {
    const client = this.requireClient();
    if (!client) throw new Error("运行时尚未连接。");
    const result = await client.command({ type: "get_provider_usage", payload: { provider_instance_id } }, { signal });
    if (client !== this.dependencies.get_client()) throw new Error("运行时连接已切换，请重新读取影响。");
    return result.payload;
  }

  async deleteProvider(provider_instance_id: ProviderInstanceId): Promise<boolean> {
    const client = this.requireClient();
    if (!client) return false;
    return this.runAction("provider:delete", async () => {
      const result = await client.command({ type: "delete_provider", payload: { provider_instance_id } });
      if (client !== this.dependencies.get_client()) throw new Error("运行时连接已切换，请重新读取设置。");
      this.invalidateModelRead();
      runInAction(() => {
        this.providers = this.providers.filter((item) => item.provider_instance_id !== provider_instance_id);
        this.showNotice(`服务商已删除，已删除 ${result.payload.fixed_config_count} 条固定配置；${result.payload.session_count} 个显式引用会话需重选模型。${result.payload.default_model ? "默认模型需重选。" : ""}${result.payload.vision_model ? "辅助识图模型需重选。" : ""}`);
      });
      await this.refreshAfterModelMutation(client);
    });
  }

  async getModelConfiguration(selection: ModelSelection, signal?: AbortSignal, origin: ModelConfigOrigin = "online"): Promise<ModelConfigurationDetail> {
    const client = this.requireClient();
    if (!client) throw new Error("运行时尚未连接。");
    const result = await client.command({ type: "get_model_configuration", payload: { ...selection, origin } }, { signal });
    if (client !== this.dependencies.get_client()) throw new Error("运行时连接已切换，请重试。");
    const detail = result.payload;
    // Dev 前端可能热更新而独立 Host 仍运行旧构建。拒绝不完整详情，不让 render 访问缺失字段。
    if (!detail || !["online", "manual"].includes(detail.origin) || !detail.field_sources || Array.isArray(detail.field_sources)
      || typeof detail.field_sources !== "object" || !detail.parameters?.tool_choice
      || Array.isArray(detail.parameters.reasoning_efforts)) {
      throw new Error("模型详情格式与当前界面不一致，请重启对应 Runtime 后重试。");
    }
    return detail;
  }

  async listFixedModels(provider_instance_id: ProviderInstanceId, offset: number, signal?: AbortSignal): Promise<ModelFixedConfig[]> {
    const client = this.requireClient();
    if (!client) throw new Error("运行时尚未连接。");
    const result = await client.command({ type: "list_fixed_model_configs", payload: { provider_instance_id, offset, limit: 20 } }, { signal });
    if (client !== this.dependencies.get_client()) throw new Error("运行时连接已切换，请重试。");
    return result.payload;
  }

  async listAllFixedModels(provider_instance_id: ProviderInstanceId, signal?: AbortSignal): Promise<ModelFixedConfig[]> {
    const records: ModelFixedConfig[] = [];
    for (let offset = 0; !signal?.aborted; offset += 20) {
      const page = await this.listFixedModels(provider_instance_id, offset, signal);
      records.push(...page);
      if (page.length < 20) break;
    }
    return records;
  }

  async saveModelFixedConfig(selection: ModelSelection, parameters: ModelParameters, origin: ModelConfigOrigin = "online"): Promise<ModelFixedConfig | null> {
    const client = this.requireClient();
    if (!client) return null;
    let saved: ModelFixedConfig | null = null;
    const applied = await this.runAction("model:save", async () => {
      const result = await client.command({ type: "save_model_fixed_config", payload: { selection, parameters, origin } });
      if (client !== this.dependencies.get_client()) throw new Error("运行时连接已切换，请重新读取配置。");
      saved = result.payload;
      this.showNotice("已固定，下次执行生效。");
      await this.refreshAfterModelMutation(client);
    });
    return applied ? saved : null;
  }

  async resetModelFixedConfig(selection: ModelSelection, origin: ModelConfigOrigin = "online"): Promise<boolean> {
    const client = this.requireClient();
    if (!client) return false;
    return this.runAction("model:reset", async () => {
      await client.command({ type: "reset_model_fixed_config", payload: selection });
      if (client !== this.dependencies.get_client()) throw new Error("运行时连接已切换，请重新读取配置。");
      this.showNotice(origin === "manual" ? "手动模型已删除。" : "固定配置已重置。");
      await this.refreshAfterModelMutation(client);
    });
  }

  async validateModel(selection: ModelSelection): Promise<ValidateModelConnectionResult | null> {
    const client = this.requireClient();
    if (!client) return null;
    let validation: ValidateModelConnectionResult | null = null;
    const applied = await this.runAction("model:validate", async () => {
      const result = await client.command({ type: "validate_model_connection", payload: { selection } });
      if (client !== this.dependencies.get_client()) throw new Error("运行时连接已切换，请重试。");
      validation = result.payload;
      if (result.payload.outcome.status === "succeeded") this.showNotice("连接测试成功。");
      else this.showError(displayConnectionFailure(result.payload.outcome.failure));
    });
    return applied ? validation : null;
  }

  private invalidateModelRead(): void {
    this.#model_read += 1;
    runInAction(() => { this.loading = false; });
  }

  private async refreshAfterModelMutation(client: RuntimeClient): Promise<void> {
    // 写命令已成功；刷新失败不能把已提交保存显示为失败并诱发重复创建。
    try { await this.dependencies.refresh_application(); }
    catch {
      if (client === this.dependencies.get_client()) this.showError("保存已完成，但刷新应用状态失败，请重新连接后核对。");
    }
  }

  private requireClient(): RuntimeClient | null {
    const client = this.dependencies.get_client();
    if (!client) this.showError("运行时尚未连接。");
    return client;
  }

  private async runAction(name: string, operation: () => Promise<void>): Promise<boolean> {
    if (this.pending_action) return false;
    const client = this.dependencies.get_client();
    runInAction(() => { this.pending_action = name; this.clearMessages(); });
    try {
      await operation();
      return client === this.dependencies.get_client();
    } catch (error: unknown) {
      runInAction(() => {
        if (client !== this.dependencies.get_client()) return;
        this.error_message = displayError(error);
        this.configuration_conflict = (error as { code?: string }).code === "configuration_conflict";
        this.permission_conflict = (error as { code?: string }).code === "permission_file_conflict";
      });
      return false;
    } finally {
      runInAction(() => {
        this.pending_action = null;
      });
    }
  }
}

function samePermissionScope(left: PermissionDocumentScope, right: PermissionDocumentScope): boolean {
  if (left.type !== right.type) return false;
  if (left.type === "global" || right.type === "global") return true;
  if (left.type === "workspace" && right.type === "workspace") {
    return left.payload.workspace_id === right.payload.workspace_id;
  }
  return left.type === "session" && right.type === "session"
    && left.payload.session_id === right.payload.session_id;
}

function displayError(error: unknown): string {
  return error instanceof Error ? error.message : "操作失败，请重试。";
}

function displayConnectionFailure(failure: ConnectionValidationFailure): string {
  const messages: Record<ConnectionValidationFailure["kind"], string> = {
    configuration: "当前模型配置无法用于连接测试，请检查协议、服务地址和模型 ID。",
    connection: "无法连接模型服务，请检查服务地址和网络状态。",
    timeout: "模型连接测试超时，请稍后重试。",
    authentication: "API Key 无效或无权访问该模型，请检查凭据。",
    model_unavailable: "当前模型不可用，请检查模型 ID 和账号权限。",
    rate_limited: "模型服务触发限流，请稍后重试。",
    service_unavailable: "模型服务暂时不可用，请稍后重试。",
    provider_rejected: "模型服务拒绝了测试请求，请检查模型配置。",
    protocol: "模型服务返回了无法识别的响应，请检查接口兼容性。",
  };
  return messages[failure.kind];
}
