import { Channel, invoke, isTauri } from "@tauri-apps/api/core";

export type BrowserEvent =
  | Readonly<{ type: "load_started" | "loaded" | "popup"; url: string }>
  | Readonly<{ type: "title"; title: string }>
  | Readonly<{ type: "notice"; message: string; url: string | null }>;
export type BrowserBounds = Readonly<{ x: number; y: number; width: number; height: number }>;
export type BrowserAction = "back" | "forward" | "reload" | "stop" | "focus";

export async function createResourceBrowser(url: string, on_event: (event: BrowserEvent) => void): Promise<string> {
  if (!isTauri()) throw new Error("请在桌面应用中打开网页。");
  const events = new Channel<BrowserEvent>();
  events.onmessage = on_event;
  return invoke("create_resource_browser", { url, events });
}

export function navigateResourceBrowser(browser_id: string, url: string): Promise<void> {
  return invoke("navigate_resource_browser", { browserId: browser_id, url });
}

export function actOnResourceBrowser(browser_id: string, action: BrowserAction): Promise<void> {
  return invoke("act_on_resource_browser", { browserId: browser_id, action });
}

export function layoutResourceBrowser(browser_id: string | null, bounds: BrowserBounds | null): Promise<void> {
  if (!isTauri()) return Promise.resolve();
  return invoke("layout_resource_browser", {
    browserId: browser_id, bounds, viewport: { width: window.innerWidth, height: window.innerHeight },
  });
}

export function resourceBrowserUrl(browser_id: string): Promise<string> {
  return invoke("resource_browser_url", { browserId: browser_id });
}

export function closeResourceBrowser(browser_id: string): Promise<void> {
  return invoke("close_resource_browser", { browserId: browser_id });
}
