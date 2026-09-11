import { invoke, isTauri } from "@tauri-apps/api/core";
import type { RuntimeHostCapabilities } from "@ez-assistant/protocol";
import { bootstrapWebRuntime } from "../runtime-client/webLogin";

export type RuntimeBootstrap = {
  readonly base_url: string;
  readonly instance_id: string;
  readonly access_token: string;
  readonly capabilities: RuntimeHostCapabilities;
  readonly started_runtime: boolean;
  readonly binding_id?: string;
  readonly target_kind?: "local" | "remote";
  readonly authentication?: "native" | "web";
};

export type RuntimeBootstrapFailure = {
  readonly code: string;
  readonly message: string;
};

export async function bootstrapRuntime(): Promise<RuntimeBootstrap> {
  if (!isTauri()) {
    return bootstrapWebRuntime();
  }
  try {
    return await invoke<RuntimeBootstrap>("bootstrap_runtime");
  } catch (error: unknown) {
    throw normalizeBootstrapFailure(error);
  }
}

function normalizeBootstrapFailure(error: unknown): RuntimeBootstrapFailure {
  if (isBootstrapFailure(error)) {
    return error;
  }
  return {
    code: "desktop_bridge_unavailable",
    message: "当前页面无法访问桌面 Runtime bridge。",
  };
}

function isBootstrapFailure(value: unknown): value is RuntimeBootstrapFailure {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const candidate = value as Record<string, unknown>;
  return typeof candidate.code === "string" && typeof candidate.message === "string";
}
