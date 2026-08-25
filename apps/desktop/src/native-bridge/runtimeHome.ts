import { invoke, isTauri } from "@tauri-apps/api/core";

type RuntimeHomeFailure = Readonly<{
  code: string;
  message: string;
}>;

export async function openRuntimeHome(): Promise<void> {
  if (!isTauri()) {
    throw new Error("浏览器预览无法打开运行时目录。");
  }
  try {
    await invoke("open_runtime_home");
  } catch (error: unknown) {
    if (isRuntimeHomeFailure(error)) {
      throw new Error(error.message);
    }
    throw new Error("无法打开运行时目录。");
  }
}

function isRuntimeHomeFailure(value: unknown): value is RuntimeHomeFailure {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as Record<string, unknown>;
  return typeof candidate.code === "string" && typeof candidate.message === "string";
}
