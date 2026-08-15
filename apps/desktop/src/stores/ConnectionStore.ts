import { action, makeObservable, observable, observableRef } from "mobx";
import type { RuntimeHostCapabilities } from "../generated/assistant-protocol";

export type RuntimeConnectionState =
  | "booting"
  | "starting_runtime"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "disconnected"
  | "component_mismatch"
  | "stopping_runtime"
  | "restarting_runtime"
  | "runtime_stopped";

export class ConnectionStore {
  state: RuntimeConnectionState = "booting";
  error_message: string | null = null;
  instance_id: string | null = null;
  capabilities: RuntimeHostCapabilities | null = null;
  address: string | null = null;
  last_connected_at_ms: number | null = null;
  last_error_code: string | null = null;
  is_stale = true;

  constructor() {
    makeObservable(this, {
      state: observable,
      error_message: observable,
      instance_id: observable,
      capabilities: observableRef,
      address: observable,
      last_connected_at_ms: observable,
      last_error_code: observable,
      is_stale: observable,
      beginInitialConnection: action,
      beginReconnect: action,
      markConnected: action,
      markDisconnected: action,
      markComponentMismatch: action,
      beginRuntimeMutation: action,
      markRuntimeStopped: action,
    });
  }

  beginInitialConnection(): void {
    this.state = "starting_runtime";
    this.error_message = null;
    this.last_error_code = null;
    this.is_stale = true;
  }

  beginReconnect(): void {
    this.state = "reconnecting";
    this.error_message = null;
    this.last_error_code = null;
    this.is_stale = true;
  }

  markConnected(
    instance_id: string,
    capabilities: RuntimeHostCapabilities,
    address: string | null = null,
  ): void {
    this.state = "connected";
    this.instance_id = instance_id;
    this.capabilities = capabilities;
    this.address = address;
    this.error_message = null;
    this.last_connected_at_ms = Date.now();
    this.last_error_code = null;
    this.is_stale = false;
  }

  markDisconnected(message: string, code = "connection_interrupted"): void {
    this.state = "disconnected";
    this.error_message = message;
    this.last_error_code = code;
    this.is_stale = true;
  }

  markComponentMismatch(message: string, code = "component_mismatch"): void {
    this.state = "component_mismatch";
    this.error_message = message;
    this.last_error_code = code;
    this.is_stale = true;
  }

  beginRuntimeMutation(kind: "stop" | "restart"): void {
    this.state = kind === "stop" ? "stopping_runtime" : "restarting_runtime";
    this.error_message = null;
    this.last_error_code = null;
    this.is_stale = true;
  }

  markRuntimeStopped(): void {
    this.state = "runtime_stopped";
    this.error_message = null;
    this.instance_id = null;
    this.address = null;
    this.last_error_code = null;
    this.is_stale = true;
  }
}
