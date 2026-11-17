import { action, makeObservable, observable, observableRef, runInAction } from "mobx";
import type { MoveSidebarItemRequest, ResetSidebarOrderRequest, SidebarPlacement } from "@ez-assistant/protocol";
import { RuntimeClientError } from "../runtime-client/RuntimeClient";
import type { RootStore } from "./RootStore";
import type { RuntimeLifecycleCoordinator } from "./RuntimeLifecycleCoordinator";

export type SidebarScope = ResetSidebarOrderRequest | Readonly<{ scope: "pins" }> | Readonly<{ scope: "desktop_pins" }>;
export const WORKSPACES: SidebarScope = { scope: "workspaces" };
export const PINS: SidebarScope = { scope: "pins" };
export const DESKTOP_PINS: SidebarScope = { scope: "desktop_pins" };

export function sessionScope(workspace_id: string | null): ResetSidebarOrderRequest {
  return { scope: "sessions", group: workspace_id === null ? { type: "unbound" } : { type: "workspace", workspace_id } };
}

export function sidebarScopeKey(scope: SidebarScope): string {
  if (scope.scope === "sessions") return JSON.stringify(["sessions", scope.group.type === "unbound" ? null : scope.group.workspace_id]);
  return scope.scope;
}

type Pending = Readonly<{ ids: readonly string[] | null; revision: number; client: unknown }>;

/** 仅保存写入期间的界面意图；权威顺序始终来自最新快照或本机偏好。 */
export class SidebarOrderStore {
  readonly pending = observable.map<string, Pending>(undefined, { deep: false });
  readonly errors = observable.map<string, string>();
  dragging: Readonly<{ key: string; item: string }> | null = null;
  #disposed = false;

  constructor(private readonly root: RootStore, private readonly runtime: (target: string | null) => RuntimeLifecycleCoordinator) {
    makeObservable(this, { dragging: observableRef, setDragging: action });
  }

  target(scope: SidebarScope): string | null {
    return scope.scope === "pins" || scope.scope === "desktop_pins" ? null : this.root.network.browsing_device;
  }

  key(scope: SidebarScope): string {
    return JSON.stringify([this.target(scope), sidebarScopeKey(scope)]);
  }

  projection(scope: SidebarScope) {
    const target = this.target(scope);
    return target ? this.root.network.context(target).projection : this.root.local_projection;
  }

  enabled(scope: SidebarScope): boolean {
    if (this.#disposed || this.root.sidebar_navigation.list_mode !== "active" || this.root.pending_session_action || this.root.pending_workspace_action || this.isPending(scope)) return false;
    if (scope.scope === "desktop_pins") return !this.root.pins.busy;
    const target = this.target(scope);
    const connection = target ? this.root.network.context(target).connection : this.root.local_connection;
    return connection.state === "connected" && !this.projection(scope).is_stale;
  }

  isPending(scope: SidebarScope): boolean {
    const pending = this.pending.get(this.key(scope));
    return !!pending && (scope.scope === "desktop_pins" || pending.client === this.runtime(this.target(scope)).client);
  }

  get visibleError(): string | undefined {
    const target = this.root.network.browsing_device;
    return [...this.errors].find(([key]) => {
      const [owner, scope] = JSON.parse(key) as [string | null, string];
      return owner === target || (owner === null && scope === "pins");
    })?.[1];
  }

  error(scope: SidebarScope): string | undefined {
    return this.errors.get(this.key(scope));
  }

  rows<T>(scope: SidebarScope, rows: readonly T[], id: (row: T) => string): readonly T[] {
    const pending = this.pending.get(this.key(scope));
    if (!pending?.ids || (scope.scope !== "desktop_pins" && (pending.revision !== this.projection(scope).sidebar_snapshot_revision || pending.client !== this.runtime(this.target(scope)).client))) return rows;
    const positions = new Map(pending.ids.map((item, index) => [item, index]));
    return [...rows].sort((a, b) => (positions.get(id(a)) ?? rows.length) - (positions.get(id(b)) ?? rows.length));
  }

  setDragging(scope: SidebarScope | null, item = ""): void {
    this.dragging = scope ? { key: this.key(scope), item } : null;
  }

  async move(scope: SidebarScope, item: string, reference: string, placement: SidebarPlacement, current: readonly string[]): Promise<boolean> {
    if (!this.enabled(scope) || item === reference || !current.includes(item) || !current.includes(reference)) return false;
    const ids = current.filter((id) => id !== item);
    ids.splice(ids.indexOf(reference) + Number(placement === "after"), 0, item);
    if (ids.every((id, index) => id === current[index])) return true;
    if (scope.scope === "desktop_pins") return this.write(scope, ids, () => this.root.pins.move(item, reference, placement));
    let payload: MoveSidebarItemRequest;
    if (scope.scope === "workspaces") payload = { ...scope, workspace_id: item, reference_id: reference, placement };
    else payload = { ...scope, session_id: item, reference_id: reference, placement };
    return this.write(scope, ids, async (runtime) => { await runtime.client!.command({ type: "move_sidebar_item", payload }); return true; });
  }

  async reset(scope: ResetSidebarOrderRequest): Promise<boolean> {
    if (!this.enabled(scope)) return false;
    return this.write(scope, null, async (runtime) => { await runtime.client!.command({ type: "reset_sidebar_order", payload: scope }); return true; });
  }

  private async write(scope: SidebarScope, ids: readonly string[] | null, write: (runtime: RuntimeLifecycleCoordinator) => Promise<boolean>): Promise<boolean> {
    const key = this.key(scope);
    const runtime = this.runtime(this.target(scope));
    const client = runtime.client;
    const projection = this.projection(scope);
    if (scope.scope !== "desktop_pins") projection.invalidateListPages();
    const pending: Pending = { ids, revision: projection.sidebar_snapshot_revision, client };
    runInAction(() => { this.pending.set(key, pending); this.errors.delete(key); });
    const current = () => !this.#disposed && this.key(scope) === key && runtime.client === client;
    try {
      const saved = await write(runtime);
      if (scope.scope !== "desktop_pins" && current()) await runtime.loadApplication();
      return saved && current();
    } catch (error) {
      if (current()) runInAction(() => { this.errors.set(key, error instanceof RuntimeClientError && error.code === "conflict" ? "列表已更新，请重试" : "顺序保存失败，请重试"); });
      // 结果不确定时只重取，绝不重发写请求。
      if (scope.scope !== "desktop_pins" && current()) await runtime.loadApplication().catch(() => undefined);
      return false;
    } finally {
      runInAction(() => { if (this.pending.get(key) === pending) this.pending.delete(key); });
    }
  }

  dispose(): void {
    this.#disposed = true;
  }
}
