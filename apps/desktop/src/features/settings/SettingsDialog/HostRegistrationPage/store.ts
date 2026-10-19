import { actionBound, makeObservable, observable, observableRef, runInAction } from "mobx";
import type { HostRegistrationCommand, HostRegistrationStatus } from "@ez-assistant/protocol";
import {
  localComputerName,
  manageHostRegistration,
  observeHostRegistration,
} from "../../../../native-bridge/hostRegistration";

/** 页面只保存状态投影与输入草稿；事件合并读取，写入后拒绝旧读取覆盖结果。 */
export class HostRegistrationStore {
  status: HostRegistrationStatus | null = null;
  name = "";
  initialName = "";
  pending = false;
  loading = false;
  error = "";
  uncertain = false;
  private active = false;
  private revision = 0;
  private refreshAgain = false;
  private stop?: () => void;

  constructor() {
    makeObservable(this, {
      status: observableRef,
      name: observable,
      initialName: observable,
      pending: observable,
      loading: observable,
      error: observable,
      uncertain: observable,
      setName: actionBound,
      refresh: actionBound,
      submit: actionBound,
    });
  }

  start() {
    this.active = true;
    void localComputerName()
      .then((name) => {
        if (this.active)
          runInAction(() => {
            if (!this.name) {
              this.name = name;
              this.initialName = name;
            }
          });
      })
      .catch(() => undefined);
    this.stop = observeHostRegistration(
      () => void this.refresh(),
      (error) => {
        if (this.active)
          runInAction(() => {
            this.error = message(error);
          });
      },
    );
    void this.refresh();
  }

  dispose() {
    this.active = false;
    this.revision++;
    this.stop?.();
  }

  setName(name: string) {
    this.name = name;
  }

  async refresh() {
    if (!this.active) return;
    if (this.pending || this.loading) {
      this.refreshAgain = true;
      return;
    }
    this.loading = true;
    const revision = this.revision;
    try {
      const status = await manageHostRegistration({ type: "get_status" });
      if (this.active && revision === this.revision) runInAction(() => this.apply(status));
    } catch (error) {
      if (this.active && revision === this.revision)
        runInAction(() => {
          this.error = message(error);
          this.uncertain = true;
        });
    } finally {
      if (this.active)
        runInAction(() => {
          this.loading = false;
          if (this.refreshAgain) {
            this.refreshAgain = false;
            void this.refresh();
          }
        });
    }
  }

  async submit(command: Exclude<HostRegistrationCommand, { type: "get_status" }>) {
    if (!this.active || this.pending || this.loading || this.uncertain) return;
    this.pending = true;
    this.error = "";
    this.revision++;
    try {
      const status = await manageHostRegistration(command);
      if (this.active) runInAction(() => this.apply(status));
    } catch (error) {
      if (this.active)
        runInAction(() => {
          this.error = `${message(error)} 请先查询最新状态。`;
          this.uncertain = true;
        });
    } finally {
      if (this.active)
        runInAction(() => {
          this.pending = false;
          if (this.refreshAgain) {
            this.refreshAgain = false;
            void this.refresh();
          }
        });
    }
  }

  private apply(status: HostRegistrationStatus) {
    // 中心查询失败时保留上次显示并明确未更新，不将空 device 当成未登记。
    const device = status.error ? (this.status?.device ?? null) : status.device;
    this.status = { ...status, device };
    this.error = status.error?.message ?? "";
    this.uncertain = !!status.error;
    if (
      !status.error &&
      status.device &&
      (["pending", "registered"].includes(status.device.status) || this.name === this.initialName)
    ) {
      this.name = status.device.name;
      this.initialName = status.device.name;
    }
  }
}

export function message(error: unknown): string {
  if (typeof error === "object" && error !== null && "message" in error && typeof error.message === "string")
    return error.message;
  return "登记操作未完成。";
}
