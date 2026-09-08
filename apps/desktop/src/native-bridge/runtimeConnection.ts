import { invoke, type InvokeArgs, type InvokeOptions } from "@tauri-apps/api/core";
import type { RuntimeBootstrap } from "./runtimeBootstrap";

let binding_id: string | null = null;
let local_target = true;
export function isLocalRuntimeTarget(): boolean { return local_target; }
export function captureRuntimeBinding(): string | null { return binding_id; }
export function clearRuntimeBinding(): void { binding_id = null; }

/** 原生凭据不在 UI 中持久化；该不透明标识只用于拒绝旧请求。 */
export async function beginRuntimeConnection(): Promise<string> {
  binding_id = null;
  return invoke<string>("begin_runtime_connection");
}
export async function connectRuntimeTarget(binding: string, origin: string | null, password: string, remember: boolean): Promise<{ bootstrap: RuntimeBootstrap; warning: string | null }> {
  const result = await invoke<{ bootstrap: RuntimeBootstrap; warning: string | null }>("connect_runtime_target", { bindingId: binding, origin, password, remember });
  return { ...result, bootstrap: { ...result.bootstrap, binding_id: binding, target_kind: origin === null ? "local" : "remote" } };
}
export function activateRuntimeBinding(bootstrap: RuntimeBootstrap): void {
  binding_id = bootstrap.binding_id ?? null;
  local_target = bootstrap.target_kind !== "remote";
}
export async function refreshRuntimeConnection(bootstrap: RuntimeBootstrap): Promise<RuntimeBootstrap> {
  const result = await invoke<RuntimeBootstrap>("refresh_runtime_connection", { bindingId: bootstrap.binding_id });
  return { ...result, binding_id: bootstrap.binding_id, target_kind: bootstrap.target_kind };
}
export function rememberedRuntimePassword(origin: string): Promise<string | null> {
  return invoke("remembered_runtime_password", { origin });
}
export function openRuntimeWeb(): Promise<void> { return invokeRuntime("open_runtime_web"); }

export async function invokeRuntime<T>(command: string, args?: InvokeArgs, options?: InvokeOptions, owner = binding_id): Promise<T> {
  if (owner !== binding_id) throw new Error("连接目标已切换，请重新操作。");
  const result = await invoke<T>(command, args, owner ? { ...options, headers: { ...options?.headers, "x-ez-runtime-binding": owner } } : options);
  if (owner !== binding_id) throw new Error("连接目标已切换，已丢弃旧操作结果。");
  return result;
}
