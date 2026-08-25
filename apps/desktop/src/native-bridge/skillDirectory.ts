import { invoke, isTauri } from "@tauri-apps/api/core";
import type { SkillSourceSnapshot, WorkspaceId } from "../generated/assistant-protocol";

type NativeFailure = Readonly<{ code: string; message: string }>;

export async function openSkillDirectory(
  source: SkillSourceSnapshot,
  workspace_id: WorkspaceId | null,
): Promise<void> {
  await invokeSkillDirectory<void>("open_skill_directory", source, workspace_id);
}

export async function copySkillDirectoryPath(
  source: SkillSourceSnapshot,
  workspace_id: WorkspaceId | null,
): Promise<void> {
  const path = await invokeSkillDirectory<string>("skill_directory_path", source, workspace_id);
  await navigator.clipboard.writeText(path);
}

async function invokeSkillDirectory<T>(
  command: string,
  source: SkillSourceSnapshot,
  workspace_id: WorkspaceId | null,
): Promise<T> {
  if (!isTauri()) throw new Error("浏览器预览无法访问本机技能来源目录。");
  try {
    return await invoke<T>(command, { source, workspaceId: workspace_id });
  } catch (error: unknown) {
    if (isNativeFailure(error)) throw new Error(error.message);
    throw new Error("无法访问技能来源目录。");
  }
}

function isNativeFailure(value: unknown): value is NativeFailure {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as Record<string, unknown>;
  return typeof candidate.code === "string" && typeof candidate.message === "string";
}
