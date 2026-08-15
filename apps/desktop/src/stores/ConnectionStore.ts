import { action, makeObservable, observable, observableRef } from "mobx";
import type { RuntimeHostCapabilities } from "../generated/assistant-protocol";

export type RuntimeConnectionState =
  | "booting"
  | "starting_runtime"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "disconnected"
  | "component_mismatch";

export class ConnectionStore {
  state: RuntimeConnectionState = "booting";
  error_message: string | null = null;
  instance_id: string | null = null;
  capabilities: RuntimeHostCapabilities | null = null;
  is_stale = true;

  constructor() {
    makeObservable(this, {
      state: observable,
      error_message: observable,
      instance_id: observable,
      capabilities: observableRef,
      is_stale: observable,
      beginInitialConnection: action,
      beginReconnect: action,
      markConnected: action,
      markDisconnected: action,
      markComponentMismatch: action,
    });
  }

  beginInitialConnection(): void {
    this.state = "starting_runtime";
    this.error_message = null;
    this.is_stale = true;
  }

  beginReconnect(): void {
    this.state = "reconnecting";
    this.error_message = null;
    this.is_stale = true;
  }

  markConnected(instance_id: string, capabilities: RuntimeHostCapabilities): void {
    this.state = "connected";
    this.instance_id = instance_id;
    this.capabilities = capabilities;
    this.error_message = null;
    this.is_stale = false;
  }

  markDisconnected(message: string): void {
    this.state = "disconnected";
    this.error_message = message;
    this.is_stale = true;
  }

  markComponentMismatch(message: string): void {
    this.state = "component_mismatch";
    this.error_message = message;
    this.is_stale = true;
  }
}
