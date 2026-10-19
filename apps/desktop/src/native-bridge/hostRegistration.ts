import { Channel, Resource } from "@tauri-apps/api/core";
import type { HostRegistrationCommand, HostRegistrationStatus } from "@ez-assistant/protocol";
import { captureRuntimeBinding, invokeRuntime } from "./runtimeConnection";

export function manageHostRegistration(command: HostRegistrationCommand): Promise<HostRegistrationStatus> {
  return invokeRuntime("manage_host_registration", { command });
}

export function localComputerName(): Promise<string> {
  return invokeRuntime("local_computer_name");
}

/** 页面订阅只观察本机 SSE；关闭只回收观察句柄，不停止 Host 的组网连接。 */
export function observeHostRegistration(changed: () => void, failed: (error: unknown) => void): () => void {
  const binding = captureRuntimeBinding();
  let active = true;
  let resource: Resource | undefined;
  let retry: ReturnType<typeof setTimeout> | undefined;
  const close = (value?: Resource) => {
    if (value) void value.close().catch(() => undefined);
  };
  async function connect() {
    const events = new Channel<{ type: "ready"; resource_id: number } | { type: "changed" }>();
    events.onmessage = (event) => {
      if (event.type === "ready") {
        const next = new Resource(event.resource_id);
        if (active) resource = next;
        else close(next);
      } else if (active) changed();
    };
    try {
      await invokeRuntime("watch_host_registration", { events }, undefined, binding);
    } catch (error) {
      if (active) failed(error);
    } finally {
      resource = undefined;
      if (active) retry = setTimeout(() => void connect(), 1500);
    }
  }
  void connect();
  return () => {
    active = false;
    clearTimeout(retry);
    close(resource);
  };
}
