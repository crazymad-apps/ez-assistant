import { action, computed, makeObservable, observable, observableRef, reaction, runInAction, type IReactionDisposer } from "mobx";
import type { HostNetworkDevice, HostNetworkResult, RuntimeEventEnvelope } from "@ez-assistant/protocol";
import { ClientResources } from "../runtime-client/ClientResources";
import { RuntimeClientError, type RuntimeClient } from "../runtime-client/RuntimeClient";
import { ComposerQuoteStore } from "./ComposerQuoteStore";
import { NewSessionDraftStore } from "./NewSessionDraftStore";
import { ResourceWorkspaceStore } from "../features/resource-workspace/ResourceWorkspaceStore";
import { ConnectionStore } from "./ConnectionStore";
import { LiveExecutionStore } from "./LiveExecutionStore";
import { NavigationStore } from "./NavigationStore";
import { RuntimeLifecycleCoordinator } from "./RuntimeLifecycleCoordinator";
import { RuntimeProjectionStore } from "./RuntimeProjectionStore";

export type DeviceWorkspace = Readonly<{
  connection: ConnectionStore;
  projection: RuntimeProjectionStore;
  live_execution: LiveExecutionStore;
  runtime: RuntimeLifecycleCoordinator;
  sidebar: NavigationStore;
  files: ClientResources;
  drafts: NewSessionDraftStore;
  quotes: ComposerQuoteStore;
  resources: ResourceWorkspaceStore;
}>;

/** 当前身份内的设备目录与读取投影。登录、设置和本机资源仍由 RootStore 唯一持有。 */
export class NetworkDevicesStore {
  available = false;
  browsing_device: string | null = null;
  devices: readonly HostNetworkDevice[] = [];
  next_offset: number | null = null;
  loading = false;
  error: string | null = null;
  self_device: string | null = null;
  readonly contexts = observable.map<string, DeviceWorkspace>({}, { deep: false });
  #revision = 0;
  #disposed = false;
  readonly #release: IReactionDisposer;

  constructor(private readonly local: () => RuntimeClient | null, private readonly navigation: NavigationStore, private readonly report: (message: string) => void, private readonly desktop: () => boolean = () => true, private readonly on_event?: (device: string, event: RuntimeEventEnvelope) => void, private readonly editing_device: () => string | null = () => null, private readonly workspace_state?: { restore: (device: string, context: DeviceWorkspace) => void; retain: () => void }) {
    makeObservable(this, {
      available: observable, browsing_device: observable, devices: observableRef,
      next_offset: observable, loading: observable, error: observable, self_device: observable,
      online_devices: computed,
      select: action, refresh: action, dispose: action,
    });
    window.addEventListener("pagehide", this.#disconnectTerminals);
    this.#release = reaction(() => [this.browsing_device, navigation.target_device, this.editing_device()], () => this.#releaseUnused());
  }

  label(device: string | null): string {
    if (device) return this.devices.find((item) => item.id === device)?.name ?? `设备 ${device.slice(0, 8)}`;
    return this.desktop() ? "本机" : this.devices.find((item) => item.id === this.self_device)?.name ?? "当前设备";
  }

  get online_devices(): readonly HostNetworkDevice[] {
    return this.devices.filter((device) => device.id !== this.self_device && device.online && device.enabled);
  }

  async refresh(append = false): Promise<void> {
    const client = this.local();
    if (!client || this.#disposed) return;
    const revision = ++this.#revision;
    const offset = append ? this.next_offset : 0;
    if (offset === null) return;
    this.loading = true;
    this.error = null;
    try {
      if (!await client.supportsNetworkProxy()) throw new Error("当前 Host 尚不支持企业组网代理。");
      const result = await client.resource<HostNetworkResult>("/host-network/access", {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ type: "list_devices", payload: { offset } }),
      }, (response) => response.json());
      if (this.#disposed || revision !== this.#revision || client !== this.local()) return;
      if (result.type !== "devices") throw new Error("设备目录响应不匹配。");
      runInAction(() => {
        this.available = true;
        this.self_device = result.payload.self_device_id;
        this.devices = [...new Map([...(append ? this.devices : []), ...result.payload.items]
          .map((item) => [item.id, item])).values()];
        this.next_offset = result.payload.next_offset;
      });
    } catch (error) {
      if (revision !== this.#revision || this.#disposed || client !== this.local()) return;
      runInAction(() => { this.error = error instanceof Error ? error.message : "设备目录读取失败。"; });
    } finally {
      if (revision === this.#revision && !this.#disposed) runInAction(() => { this.loading = false; });
    }
  }

  select(device: string | null): void {
    if (device === this.self_device) device = null;
    if (device && (!this.available || !this.devices.some((item) => item.id === device))) return;
    if (device) this.context(device);
    this.browsing_device = device;
    if (device) void this.ensureConnected(device);
  }

  context(device: string): DeviceWorkspace {
    const existing = this.contexts.get(device);
    if (existing) return existing;
    const connection = new ConnectionStore();
    const projection = new RuntimeProjectionStore();
    const live_execution = new LiveExecutionStore();
    const sidebar = new NavigationStore();
    const runtime = new RuntimeLifecycleCoordinator({
      on_event: (event) => this.on_event?.(device, event),
      connection, projection, live_execution, navigation: this.navigation, target_device: device,
      create_client: async () => {
        const local = this.local();
        if (!local || this.#disposed) throw new RuntimeClientError("runtime_unavailable", "本机连接不可用。");
        const client = await local.forDevice(device);
        if (local !== this.local() || this.#disposed) { client.dispose(); throw new Error("本机连接已变化。"); }
        return client;
      },
      report_interaction_error: (message) => { if (this.navigation.target_device === device) this.report(message); },
      refresh_device_gateway: () => undefined,
      mark_device_gateway_stale: () => undefined,
      on_title_generation_finished: () => undefined,
    });
    const context = { connection, projection, live_execution, sidebar, runtime, files: new ClientResources(() => runtime.client, false), drafts: new NewSessionDraftStore(), quotes: new ComposerQuoteStore(), resources: new ResourceWorkspaceStore(() => runtime.client, (id) => this.devices.some((device) => device.id === id && device.desktop_control)) };
    this.workspace_state?.restore(device, context);
    runInAction(() => this.contexts.set(device, context));
    return context;
  }

  async ensureConnected(device: string): Promise<void> {
    const context = this.context(device);
    if (context.connection.state !== "connected") await context.runtime.connect();
  }

  #releaseUnused(): void {
    for (const [device, context] of this.contexts) {
      if (device === this.browsing_device || device === this.navigation.target_device || device === this.editing_device()
        || context.drafts.drafts.size > 0 || context.resources.groups.size > 1
        || context.resources.tabs.length > 1) continue;
      this.workspace_state?.retain();
      context.runtime.dispose();
      context.files.dispose();
      context.resources.dispose();
      runInAction(() => this.contexts.delete(device));
    }
  }

  #disconnectTerminals = (): void => {
    for (const context of this.contexts.values()) context.resources.disconnectTerminals();
  };

  dispose(): void {
    window.removeEventListener("pagehide", this.#disconnectTerminals);
    this.#disposed = true;
    this.#revision++;
    this.#release();
    for (const context of this.contexts.values()) {
      for (const draft of context.drafts.clear()) for (const attachment of draft.attachments) {
        if (attachment.state !== "uploaded") void context.files.releaseAttachmentSelection(attachment.selection_id).catch(() => undefined);
      }
      context.resources.dispose(); context.runtime.dispose(); context.files.dispose();
    }
    this.contexts.clear();
    this.devices = [];
  }
}
