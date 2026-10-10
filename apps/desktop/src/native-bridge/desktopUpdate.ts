import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { HostMode } from "@ez-assistant/protocol";
export interface DesktopUpdateSnapshot {
  revision: number;
  phase: "idle" | "available" | "downloading" | "installing" | "failed";
  available: boolean;
  required: boolean;
  notes: string;
  received: number;
  total: number;
  error: string | null;
  resume_remote: string | null;
  stops_local_host: boolean;
  binding_id: string | null;
  host_mode: HostMode | null;
}


export function readDesktopUpdate(): Promise<DesktopUpdateSnapshot> { return invoke("desktop_update_state"); }

export function listenDesktopUpdate(receive: (snapshot: DesktopUpdateSnapshot) => void): Promise<UnlistenFn> {
  return listen<DesktopUpdateSnapshot>("desktop-update-state", ({ payload }) => receive(payload));
}

export function startDesktopUpdate(): Promise<void> { return invoke("start_desktop_update"); }

export function cancelDesktopUpdate(): Promise<void> { return invoke("cancel_desktop_update"); }
