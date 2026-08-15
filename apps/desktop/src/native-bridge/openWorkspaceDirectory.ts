import { invoke, isTauri } from "@tauri-apps/api/core";
import type { WorkspaceId } from "../generated/assistant-protocol";

type WorkspaceOpenFailure = {
  readonly code: string;
  readonly message: string;
};

export async function openWorkspaceDirectory(workspace_id: WorkspaceId): Promise<void> {
  if (!isTauri()) {
    throw new Error("浏览器预览无法打开本机工作空间目录。");
  }
  try {
    await invoke("open_workspace_directory", { workspaceId: workspace_id });
  } catch (error: unknown) {
    if (isWorkspaceOpenFailure(error)) {
      throw new Error(error.message);
    }
    throw new Error("无法打开工作空间目录。");
  }
}

function isWorkspaceOpenFailure(value: unknown): value is WorkspaceOpenFailure {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const candidate = value as Record<string, unknown>;
  return typeof candidate.code === "string" && typeof candidate.message === "string";
}
