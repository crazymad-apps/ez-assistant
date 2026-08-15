import { invoke, isTauri } from "@tauri-apps/api/core";

type WorkspaceDirectoryFailure = {
  readonly code: string;
  readonly message: string;
};

export async function chooseWorkspaceDirectory(): Promise<string | null> {
  if (!isTauri()) {
    throw new Error("浏览器预览未连接目录选择器。");
  }
  try {
    return await invoke<string | null>("choose_workspace_directory");
  } catch (error: unknown) {
    throw normalizeWorkspaceDirectoryFailure(error);
  }
}

function normalizeWorkspaceDirectoryFailure(error: unknown): Error {
  if (isWorkspaceDirectoryFailure(error)) {
    return new Error(error.message);
  }
  return new Error("无法打开工作空间目录选择器。");
}

function isWorkspaceDirectoryFailure(value: unknown): value is WorkspaceDirectoryFailure {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const candidate = value as Record<string, unknown>;
  return typeof candidate.code === "string" && typeof candidate.message === "string";
}
