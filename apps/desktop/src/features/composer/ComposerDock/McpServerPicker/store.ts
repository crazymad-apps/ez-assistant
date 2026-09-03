import { action, makeObservable, observable, observableRef, runInAction } from "mobx";
import type { McpServerOptionSnapshot } from "../../../../generated/assistant-protocol";

/** 只保存本次打开的服务摘要；关闭后失效，绝不缓存工具目录或授权结论。 */
export class McpServerPickerStore {
  servers: readonly McpServerOptionSnapshot[] = [];
  loading = false;
  error: string | null = null;
  #generation = 0;

  constructor() {
    makeObservable(this, { servers: observableRef, loading: observable, error: observable, load: action, dispose: action });
  }

  async load(query: () => Promise<readonly McpServerOptionSnapshot[]>): Promise<void> {
    const generation = ++this.#generation;
    this.servers = [];
    this.loading = true;
    this.error = null;
    try {
      const servers = await query();
      runInAction(() => {
        if (generation === this.#generation) this.servers = servers;
      });
    } catch (error: unknown) {
      runInAction(() => {
        if (generation === this.#generation) this.error = error instanceof Error ? error.message : "无法读取 MCP 服务";
      });
    } finally {
      runInAction(() => {
        if (generation === this.#generation) this.loading = false;
      });
    }
  }

  dispose(): void {
    ++this.#generation;
    this.servers = [];
    this.loading = false;
    this.error = null;
  }
}
