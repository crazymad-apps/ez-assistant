import { invoke } from "@tauri-apps/api/core";

export async function openExternalHttpUrl(url: string): Promise<void> {
  const parsed = new URL(url);
  if ((parsed.protocol !== "http:" && parsed.protocol !== "https:") || !parsed.hostname) {
    throw new Error("只允许打开有效的 HTTP 或 HTTPS 链接。");
  }
  await invoke("open_external_http_url", { url: parsed.toString() });
}
