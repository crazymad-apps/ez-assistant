import { action, makeObservable, observable, observableRef, runInAction } from "mobx";
import type {
  ConfigurationStatus,
  ConnectionValidationFailure,
  ModelConfiguration,
  ModelConfigurationInput,
  ModelCatalogSnapshot,
  ModelKey,
  PermissionDocumentDraft,
  PermissionDocumentRevision,
  PermissionDocumentScope,
  PermissionDocumentSnapshot,
  SessionId,
  ValidateModelConnectionResult,
  WorkspaceId,
} from "../generated/assistant-protocol";
import type { RuntimeClient } from "../runtime-client/RuntimeClient";

export type SettingsPage = "runtime" | "models" | "permissions" | "memory";

type SettingsDependencies = Readonly<{
  get_client: () => RuntimeClient | null;
  get_permission_context: () => Readonly<{
    session_id: SessionId | null;
    workspace_id: WorkspaceId | null;
  }>;
  refresh_application: () => Promise<void>;
}>;

export class SettingsStore {
  is_open = false;
  page: SettingsPage = "runtime";
  loading = false;
  pending_action: string | null = null;
  status: ConfigurationStatus | null = null;
  models: readonly ModelConfiguration[] = [];
  model_catalog: ModelCatalogSnapshot | null = null;
  error_message: string | null = null;
  notice_message: string | null = null;
  configuration_conflict = false;
  permission_documents: readonly PermissionDocumentSnapshot[] = [];
  permission_conflict = false;

  constructor(private readonly dependencies: SettingsDependencies) {
    makeObservable(this, {
      is_open: observable,
      page: observable,
      loading: observable,
      pending_action: observable,
      status: observableRef,
      models: observableRef,
      model_catalog: observableRef,
      error_message: observable,
      notice_message: observable,
      configuration_conflict: observable,
      permission_documents: observableRef,
      permission_conflict: observable,
      open: action,
      close: action,
      selectPage: action,
      load: action,
      reloadConfiguration: action,
      loadPermissions: action,
      reloadPermissions: action,
      replacePermissionDocument: action,
      createModel: action,
      updateModel: action,
      deleteModel: action,
      setDefaultModel: action,
      setAuxiliaryVisionModel: action,
      validateConfigured: action,
      validateCandidate: action,
      clearMessages: action,
      showError: action,
      showNotice: action,
    });
  }

  open(page: SettingsPage = "runtime"): void {
    this.is_open = true;
    this.page = page;
    void this.load();
    if (page === "permissions") void this.loadPermissions();
  }

  close(): void {
    this.is_open = false;
    this.error_message = null;
    this.notice_message = null;
    this.configuration_conflict = false;
    this.permission_conflict = false;
  }

  selectPage(page: SettingsPage): void {
    this.page = page;
    this.clearMessages();
    if (page === "permissions") void this.loadPermissions();
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
      this.error_message = "Runtime 尚未连接。";
      return;
    }
    this.loading = true;
    this.error_message = null;
    try {
      const [status, models] = await Promise.all([
        client.command({ type: "get_config_status", payload: {} }),
        client.command({ type: "list_models", payload: {} }),
      ]);
      runInAction(() => {
        this.status = status.payload.status;
        this.models = models.payload.models;
        this.model_catalog = models.payload.catalog;
        this.configuration_conflict = false;
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

  async reloadConfiguration(): Promise<void> {
    const client = this.requireClient();
    if (!client) return;
    await this.runAction("reload", async () => {
      const result = await client.command({ type: "reload_config", payload: {} });
      const models = await client.command({ type: "list_models", payload: {} });
      this.status = result.payload.status;
      this.models = models.payload.models;
      this.model_catalog = models.payload.catalog;
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

  async createModel(model: ModelConfigurationInput, set_default: boolean): Promise<boolean> {
    const client = this.requireClient();
    if (!client) return false;
    return this.runAction("create", async () => {
      const result = await client.command({
        type: "create_model",
        payload: {
          model,
          expected_revision: this.status?.revision ?? null,
          set_default,
        },
      });
      this.applyMutation(result.payload);
      this.notice_message = "模型已添加。";
      await this.dependencies.refresh_application();
    });
  }

  async updateModel(model: ModelConfigurationInput, set_default: boolean): Promise<boolean> {
    const client = this.requireClient();
    const revision = this.status?.revision;
    if (!client || !revision) {
      this.error_message = "请先重新加载配置。";
      return false;
    }
    return this.runAction("update", async () => {
      const result = await client.command({
        type: "update_model",
        payload: { model, expected_revision: revision, set_default },
      });
      this.applyMutation(result.payload);
      this.notice_message = "模型已保存。";
      await this.dependencies.refresh_application();
    });
  }

  async deleteModel(model_key: ModelKey, replacement_default: ModelKey | null): Promise<boolean> {
    const client = this.requireClient();
    const revision = this.status?.revision;
    if (!client || !revision) return false;
    return this.runAction("delete", async () => {
      const result = await client.command({
        type: "delete_model",
        payload: { model_key, expected_revision: revision, replacement_default },
      });
      this.applyMutation(result.payload);
      this.notice_message = "模型已删除。";
      await this.dependencies.refresh_application();
    });
  }

  async setDefaultModel(model_key: ModelKey): Promise<boolean> {
    const client = this.requireClient();
    const revision = this.status?.revision;
    if (!client || !revision) return false;
    return this.runAction("default", async () => {
      const result = await client.command({
        type: "set_default_model",
        payload: { model_key, expected_revision: revision },
      });
      this.applyMutation(result.payload);
      this.notice_message = "默认模型已更新。";
      await this.dependencies.refresh_application();
    });
  }

  async setAuxiliaryVisionModel(model_key: ModelKey | null): Promise<boolean> {
    const client = this.requireClient();
    const revision = this.status?.revision;
    if (!client || !revision) {
      this.error_message = "请先重新加载配置。";
      return false;
    }
    return this.runAction("vision-model", async () => {
      const result = await client.command({
        type: "set_auxiliary_vision_model",
        payload: { model_key, expected_revision: revision },
      });
      this.applyMutation(result.payload);
      this.notice_message = model_key ? "默认识图模型已更新。" : "默认识图模型已清除。";
      await this.dependencies.refresh_application();
    });
  }

  async validateCandidate(model: ModelConfigurationInput): Promise<ValidateModelConnectionResult | null> {
    const client = this.requireClient();
    if (!client) return null;
    let validation: ValidateModelConnectionResult | null = null;
    const succeeded = await this.runAction("validate", async () => {
      const result = await client.command({
        type: "validate_model_connection",
        payload: { target: { type: "candidate", payload: model } },
      });
      validation = result.payload;
      if (result.payload.outcome.status === "succeeded") {
        this.showNotice("连接测试成功。");
      } else {
        this.showError(displayConnectionFailure(result.payload.outcome.failure));
      }
    });
    return succeeded ? validation : null;
  }

  async validateConfigured(model_key: ModelKey): Promise<ValidateModelConnectionResult | null> {
    const client = this.requireClient();
    if (!client) return null;
    let validation: ValidateModelConnectionResult | null = null;
    const succeeded = await this.runAction(`validate:${model_key}`, async () => {
      const result = await client.command({
        type: "validate_model_connection",
        payload: { target: { type: "configured", payload: { model_key } } },
      });
      validation = result.payload;
      if (result.payload.outcome.status === "succeeded") {
        this.showNotice(`模型“${model_key}”连接测试成功。`);
      } else {
        this.showError(displayConnectionFailure(result.payload.outcome.failure));
      }
    });
    return succeeded ? validation : null;
  }

  private applyMutation(result: { status: ConfigurationStatus; models: ModelConfiguration[] }): void {
    this.status = result.status;
    this.models = result.models;
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
      return true;
    } catch (error: unknown) {
      runInAction(() => {
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
    configuration: "当前模型配置无法用于连接测试，请检查协议、Endpoint 和模型 ID。",
    connection: "无法连接模型服务，请检查 Endpoint 和网络状态。",
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
