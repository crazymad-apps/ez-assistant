import { action, makeObservable, observable, observableRef, runInAction } from 'mobx';
import type { McpServerTarget, McpUiResource, McpUiResourceSummary } from '@ez-assistant/protocol';
import type { RuntimeClient } from '../../../../../runtime-client/RuntimeClient';

/** 设置页短期资源投影；换域或目录刷新即清空，HTML 不进入偏好或会话。 */
export class UiResourcesStore {
  resources: readonly McpUiResourceSummary[] | null = null;
  resource: McpUiResource | null = null;
  pending = false;
  error: string | null = null;
  #request: AbortController | null = null;

  constructor(
    private readonly client: RuntimeClient,
    private readonly target: McpServerTarget,
  ) {
    makeObservable(this, {
      resources: observableRef,
      resource: observableRef,
      pending: observable,
      error: observable,
      load: action,
      read: action,
      dispose: action,
    });
  }

  async load(): Promise<void> {
    const request = this.#begin();
    try {
      const result = await this.client.command(
        { type: 'list_mcp_ui_resources', payload: { target: this.target } },
        { signal: request.signal },
      );
      if (this.#request === request)
        runInAction(() => {
          this.resources = result.payload.resources;
        });
    } catch {
      if (this.#request === request)
        runInAction(() => {
          this.error = '界面资源读取失败，请刷新服务后重试';
        });
    } finally {
      if (this.#request === request)
        runInAction(() => {
          this.pending = false;
        });
    }
  }

  async read(uri: string): Promise<void> {
    const request = this.#begin();
    try {
      const result = await this.client.command(
        { type: 'read_mcp_ui_resource', payload: { target: this.target, uri } },
        { signal: request.signal },
      );
      if (this.#request === request)
        runInAction(() => {
          this.resource = result.payload.resource;
        });
    } catch {
      if (this.#request === request)
        runInAction(() => {
          this.error = '界面资源不可用，请刷新服务后重试';
        });
    } finally {
      if (this.#request === request)
        runInAction(() => {
          this.pending = false;
        });
    }
  }

  dispose(): void {
    this.#request?.abort();
    this.#request = null;
    this.resource = null;
    this.resources = null;
    this.pending = false;
    this.error = null;
  }

  #begin(): AbortController {
    this.#request?.abort();
    const request = new AbortController();
    this.#request = request;
    this.resource = null;
    this.error = null;
    this.pending = true;
    return request;
  }
}
