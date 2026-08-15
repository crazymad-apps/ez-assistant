import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { RuntimeBootstrap } from "./runtimeBootstrap";

export type DesktopPlatform = "macos" | "linux" | "unsupported";
export type DesktopLifecycleIntent = "quit_desktop" | "stop_runtime" | "restart_runtime";
export type NativeRuntimeState =
  | "connecting"
  | "connected"
  | "reconnecting"
  | "disconnected"
  | "stopping"
  | "restarting"
  | "stopped";
export type NativeRuntimeImpact = Readonly<{
  active_runs: number;
  queued_inputs: number;
  pending_approvals: number;
}>;
export type NativeRuntimeMutationEvent =
  | Readonly<{ phase: "preparing"; kind: "stop" | "restart" }>
  | Readonly<{ phase: "finished"; kind: "stop" | "restart"; succeeded: boolean }>;

export async function getDesktopPlatform(): Promise<DesktopPlatform> {
  if (!isTauri()) return "unsupported";
  return invoke<DesktopPlatform>("desktop_platform");
}

export async function listenDesktopLifecycleIntents(
  listener: (intent: DesktopLifecycleIntent) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) return () => undefined;
  return listen<DesktopLifecycleIntent>("desktop://lifecycle-intent", (event) => listener(event.payload));
}

export async function listenNativeRuntimeMutations(
  listener: (event: NativeRuntimeMutationEvent) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) return () => undefined;
  return listen<NativeRuntimeMutationEvent>("desktop://native-runtime-mutation", (event) => {
    listener(event.payload);
  });
}

export async function takePendingDesktopLifecycleIntent(): Promise<DesktopLifecycleIntent | null> {
  if (!isTauri()) return null;
  return invoke<DesktopLifecycleIntent | null>("take_pending_desktop_lifecycle_intent");
}

export async function updateNativeRuntimeState(
  state: NativeRuntimeState,
  impact?: NativeRuntimeImpact,
): Promise<void> {
  if (!isTauri()) return;
  await invoke("update_native_runtime_state", { state, impact });
}

export async function stopNativeRuntime(): Promise<void> {
  if (!isTauri()) return;
  await invoke("stop_runtime");
}

export async function restartNativeRuntime(): Promise<RuntimeBootstrap | null> {
  if (!isTauri()) return null;
  return invoke<RuntimeBootstrap>("restart_runtime");
}

export async function quitDesktopClient(): Promise<void> {
  if (!isTauri()) return;
  await invoke("quit_desktop");
}

export async function minimizeDesktopWindow(): Promise<void> {
  if (!isTauri()) return;
  await invoke("minimize_desktop_window");
}

export async function toggleMaximizeDesktopWindow(): Promise<boolean> {
  if (!isTauri()) return false;
  return invoke<boolean>("toggle_maximize_desktop_window");
}

export async function isDesktopWindowMaximized(): Promise<boolean> {
  if (!isTauri()) return false;
  return invoke<boolean>("is_desktop_window_maximized");
}

export async function listenDesktopWindowMaximized(
  listener: (maximized: boolean) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) return () => undefined;
  return listen<boolean>("desktop://window-maximized", (event) => listener(event.payload));
}

export async function requestDesktopClose(): Promise<void> {
  if (!isTauri()) return;
  await invoke("request_desktop_close");
}
