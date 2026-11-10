import { action, makeObservable, observable, observableRef, runInAction } from 'mobx';
import type { RuntimeClient } from '../../runtime-client/RuntimeClient';
import type { PluginUiCapability, ReadUiAppResult, UiAppScope } from '@ez-assistant/protocol';
import type { AppPageSource } from '../mcp-apps';
import { type AppTarget, AppLinkEndpoint } from '../mcp-apps/appLinks';
import { releaseAppToolPage, type AppToolPage } from '../mcp-apps/appTools';

/** 单标签的短期页面投影；恢复只保存入口，不保存参数、代码、关联或外部来源许可。 */
export class AppController {
  source: AppPageSource | null = null;
  title: string;
  icon_data_url: string | null = null;
  error: string | null = null;
  pending = false;
  capabilities: PluginUiCapability[] = [];
  tool_page: AppToolPage | undefined;
  #scope: UiAppScope = { type: 'draft' };
  #generation = 0;
  #identity = '';
  #request: AbortController | null = null;
  #endpoint: AppLinkEndpoint | null = null;
  readonly #waiters = new Set<(endpoint: AppLinkEndpoint | null) => void>();
  constructor(
    readonly target: AppTarget,
    readonly instance_key: string | undefined,
    public arguments_value: Record<string, unknown>,
    private readonly getClient: () => RuntimeClient | null,
  ) {
    this.title = target.type === 'external' ? new URL(target.url).hostname : target.entry_id;
    makeObservable(this, {
      source: observableRef,
      title: observable,
      icon_data_url: observable,
      error: observable,
      pending: observable,
      capabilities: observableRef,
      tool_page: observableRef,
      load: action,
      disconnect: action,
    });
  }
  get origin(): string {
    if (this.target.type === 'external') return new URL(this.target.url).origin;
    if (this.target.type === 'plugin') return this.target.name;
    return this.target.target.server_key;
  }
  async load(approve_external = false, scope = this.#scope): Promise<void> {
    this.disconnect(true);
    this.#scope = scope;
    const generation = this.#generation;
    const client = this.getClient();
    if (!client) return;
    this.error = null;
    if (this.target.type === 'external') {
      if (!approve_external) return;
      this.capabilities = ['open-app', 'post-message', 'set-composer-draft'];
      this.source = {
        kind: 'external',
        url: this.target.url,
        approved_origin: new URL(this.target.url).origin,
        host_origins: [new URL(client.address).origin, location.origin],
      };
      return;
    }
    const request = new AbortController();
    this.#request = request;
    this.pending = true;
    try {
      const result = await client.command(
        { type: 'read_ui_app', payload: { target: this.target, arguments: this.arguments_value, mount: scope } },
        { signal: request.signal },
      );
      if (generation !== this.#generation || request.signal.aborted || this.getClient() !== client) {
        if (result.payload.tool_instance_id)
          releaseAppToolPage({ client, instance_id: result.payload.tool_instance_id });
        return;
      }
      this.apply(result.payload, client);
    } catch {
      if (generation === this.#generation)
        runInAction(() => {
          this.error = '应用不可用：入口缺失、已禁用、配置不全或参数不兼容。';
        });
    } finally {
      if (generation === this.#generation)
        runInAction(() => {
          this.pending = false;
        });
    }
  }
  /** 每次复用均由 Runtime 校验定位参数；身份变化才更换代码，正常定位保持原页面。 */
  async locate(arguments_value: Record<string, unknown>): Promise<void> {
    if (!this.source) {
      this.arguments_value = arguments_value;
      if (this.pending || this.error) {
        await this.load();
        if (this.error) throw new Error(this.error);
      }
      return;
    }
    if (this.target.type === 'external') {
      this.arguments_value = arguments_value;
      return;
    }
    const client = this.getClient();
    const generation = this.#generation;
    if (!client) throw new Error('应用连接已失效');
    const result = await client.command({
      type: 'read_ui_app',
      payload: { target: this.target, arguments: arguments_value },
    });
    if (generation !== this.#generation || client !== this.getClient()) throw new Error('应用连接已失效');
    this.arguments_value = arguments_value;
    const identity = `${result.payload.source_identity}:${JSON.stringify(result.payload.entry)}`;
    if (identity !== this.#identity) {
      await this.load();
      if (this.error) throw new Error(this.error);
    }
  }
  private apply(result: ReadUiAppResult, client: RuntimeClient): void {
    this.#identity = `${result.source_identity}:${JSON.stringify(result.entry)}`;
    runInAction(() => {
      this.title = result.entry.title;
      this.icon_data_url = result.icon_data_url ?? null;
      this.capabilities = result.entry.capabilities;
      this.tool_page = result.tool_instance_id ? { client, instance_id: result.tool_instance_id } : undefined;
      this.source = { kind: 'resource', resource: result.resource, proxy_url: `${client.address}/ui/sandbox-proxy` };
    });
  }
  attach(endpoint: AppLinkEndpoint): void {
    this.#endpoint?.retire();
    this.#endpoint = endpoint;
    for (const done of this.#waiters) done(endpoint);
    this.#waiters.clear();
  }
  async endpoint(): Promise<AppLinkEndpoint> {
    if (this.#endpoint?.live) return this.#endpoint;
    return new Promise((resolve, reject) => {
      const done = (endpoint: AppLinkEndpoint | null) => {
        clearTimeout(timer);
        this.#waiters.delete(done);
        if (endpoint) resolve(endpoint);
        else reject(new Error('应用页面未连接'));
      };
      const timer = setTimeout(() => done(null), 60_000);
      this.#waiters.add(done);
    });
  }
  disconnect(keep_waiters = false): void {
    this.#generation++;
    this.#request?.abort();
    this.#request = null;
    this.#endpoint?.retire();
    this.#endpoint = null;
    if (!keep_waiters) {
      for (const done of this.#waiters) done(null);
      this.#waiters.clear();
    }
    this.source = null;
    this.icon_data_url = null;
    if (this.tool_page) releaseAppToolPage(this.tool_page);
    this.tool_page = undefined;
    this.pending = false;
  }
}
