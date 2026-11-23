import { makeAutoObservable, observable, runInAction } from "mobx";
import type { BackgroundRunSummary, SessionId, StopBackgroundRunResult } from "@ez-assistant/protocol";
import { RuntimeClientError, type RuntimeClient } from "../../../runtime-client/RuntimeClient";

export const BACKGROUND_PAGE_SIZE = 5;

function endedMessage(result?: StopBackgroundRunResult): string {
  if (!result) return "后台命令已结束，输出已不可用。";
  if (result.output_error) return "后台命令已结束，输出处理遇到问题。";
  if (result.timed_out) return "后台命令已超时结束，输出已清理。";
  if (result.exit_code != null && result.exit_code !== 0) return `后台命令已结束，退出码 ${result.exit_code}。`;
  return "后台命令已结束，输出已清理。";
}

export class BackgroundRunsStore {
  rows: BackgroundRunSummary[] = [];
  page = 0;
  total: number | null = null;
  loading = false;
  error: string | null = null;
  notice: Readonly<{ background_id: string; message: string }> | null = null;
  selected: BackgroundRunSummary | null = null;
  readonly stopping = observable.set<string>();
  readonly stop_errors = observable.map<string, string>();

  #life: AbortController | null = null;
  #query: AbortController | null = null;
  #available = false;
  #requested_page = 0;
  constructor(readonly client: RuntimeClient, readonly session_id: SessionId) {
    makeAutoObservable(this, { client: false, session_id: false });
  }

  attach(): void {
    this.#life = new AbortController();
  }

  dispose(): void {
    this.#life?.abort();
    this.#life = null;
    this.#query?.abort();
    this.#query = null;
    this.#available = false;
    this.stopping.clear();
  }

  setAvailable(available: boolean): void {
    this.#available = available;
    if (available) {
      void this.load();
      return;
    }
    this.#query?.abort();
    this.loading = false;
    this.notice = null;
  }

  changed(id: string): void {
    if (this.notice?.background_id !== id) this.notice = null;
    void this.load();
  }

  async load(page = this.#requested_page): Promise<void> {
    if (!this.#life || !this.#available) return;
    this.#requested_page = page;
    this.#query?.abort();
    const request = new AbortController();
    this.#query = request;
    this.loading = true;
    this.error = null;
    try {
      // 删除可同时影响总数与页码。先重取服务器给出的最后有效页，再一次安装数据与页码。
      for (;;) {
        const result = await this.client.listBackgroundRuns({
          session_id: this.session_id, limit: BACKGROUND_PAGE_SIZE, offset: page * BACKGROUND_PAGE_SIZE,
        }, request.signal);
        if (request.signal.aborted) return;
        const last_page = Math.max(0, Math.ceil(result.total / BACKGROUND_PAGE_SIZE) - 1);
        if (page > last_page && result.total > 0) {
          page = last_page;
          this.#requested_page = page;
          continue;
        }
        runInAction(() => {
          this.rows = result.runs;
          this.total = result.total;
          this.page = Math.min(page, last_page);
          this.#requested_page = this.page;
        });
        return;
      }
    } catch (error) {
      if (!request.signal.aborted) runInAction(() => {
        this.error = error instanceof Error ? error.message : "无法读取后台运行。";
      });
    } finally {
      if (!request.signal.aborted) runInAction(() => { this.loading = false; });
    }
  }

  open(row: BackgroundRunSummary): void {
    this.notice = null;
    this.selected = row;
  }

  close(): void { this.selected = null; }

  ended(id: string, result?: StopBackgroundRunResult): void {
    const was_selected = this.selected?.background_id === id;
    // 同一次停止可能先收到结束事件再收到应答。只有尚在展示的提示能被后续应答补充。
    const was_stopping = this.stopping.delete(id);
    if (was_selected) this.close();
    if (this.#available && (was_selected || was_stopping || (result && this.notice?.background_id === id))) {
      this.notice = { background_id: id, message: endedMessage(result) };
    }
    this.stop_errors.delete(id);
    this.rows = this.rows.filter((row) => row.background_id !== id);
    void this.load();
  }

  async stop(id: string): Promise<void> {
    const scope = this.#life?.signal;
    if (!scope || this.stopping.has(id)) return;
    this.stopping.add(id);
    this.stop_errors.delete(id);
    this.notice = null;
    try {
      const result = await this.client.stopBackgroundRun({ session_id: this.session_id, background_id: id });
      if (!scope.aborted) this.ended(id, result);
    } catch (error) {
      if (scope.aborted) return;
      if (error instanceof RuntimeClientError && error.code === "background_run_not_found") {
        this.ended(id);
        return;
      }
      runInAction(() => {
        this.stop_errors.set(id, error instanceof Error ? error.message : "无法确认后台命令已停止。");
      });
      // 网络或回收错误只回查状态，重试停止由用户发起。
      void this.load();
    } finally {
      if (!scope.aborted) runInAction(() => { this.stopping.delete(id); });
    }
  }
}
