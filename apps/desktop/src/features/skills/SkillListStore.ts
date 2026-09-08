import { action, makeObservable, observable, observableRef, runInAction } from "mobx";
import type { SkillManagementSnapshot } from "../../generated/assistant-protocol";

/** 单个可见列表的查询结果；关闭/换 owner 后丢弃，不进入会话或持久存储。 */
export class SkillListStore {
  snapshot: SkillManagementSnapshot | null = null;
  loading = false;
  error: string | null = null;
  #generation = 0;

  constructor() {
    makeObservable(this, { snapshot: observableRef, loading: observable, error: observable, load: action, dispose: action });
  }

  async load(query: () => Promise<SkillManagementSnapshot>): Promise<void> {
    const generation = ++this.#generation;
    this.snapshot = null;
    this.error = null;
    this.loading = true;
    try {
      const snapshot = await query();
      runInAction(() => {
        if (generation !== this.#generation) return;
        this.snapshot = snapshot;
        if (!snapshot.available) this.error = "当前技能列表不可用，请重试";
      });
    } catch (failure: unknown) {
      runInAction(() => {
        if (generation === this.#generation) this.error = failure instanceof Error ? failure.message : "无法读取技能列表";
      });
    } finally {
      runInAction(() => { if (generation === this.#generation) this.loading = false; });
    }
  }

  dispose(): void {
    ++this.#generation;
    this.snapshot = null;
    this.loading = false;
    this.error = null;
  }
}
