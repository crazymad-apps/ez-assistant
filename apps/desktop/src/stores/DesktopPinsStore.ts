import { action, makeObservable, observable, observableRef, runInAction } from "mobx";
import type { SessionSummary } from "@ez-assistant/protocol";
import type { RuntimeClient } from "../runtime-client/RuntimeClient";

export type DesktopPin = Readonly<{ device: string | null; session_id: string; title: string; device_name: string }>;
export type DesktopPins = Readonly<{ imported: boolean; items: readonly DesktopPin[] }>;

/** 本机快捷入口；只有偏好写入成功才更新可见状态，绝不回写 Runtime 置顶。 */
export class DesktopPinsStore {
  value: DesktopPins = { imported: false, items: [] };
  busy = false;
  error: string | null = null;
  #disposed = false;

  constructor(private readonly save: (value: DesktopPins) => Promise<void>) {
    makeObservable(this, { value: observableRef, busy: observable, error: observable, load: action });
  }

  load(value: DesktopPins): void { this.value = value; }

  has(session_id: string, device: string | null): boolean {
    return this.value.items.some((item) => item.device === device && item.session_id === session_id);
  }

  async set(pin: DesktopPin, enabled: boolean): Promise<boolean> {
    if (this.busy || this.#disposed) return false;
    const items = this.value.items.filter((item) => item.device !== pin.device || item.session_id !== pin.session_id);
    return this.#commit({ ...this.value, items: enabled ? [...items, pin] : items });
  }

  async importLegacy(client: RuntimeClient, local: boolean): Promise<void> {
    if (this.busy || this.value.imported || this.#disposed) return;
    runInAction(() => { this.busy = true; this.error = null; });
    try {
      const pins = new Map(this.value.items.map((pin) => [JSON.stringify([pin.device, pin.session_id]), pin]));
      if (local) {
        let offset = 0;
        for (;;) {
          const page = await client.command({ type: "list_sessions", payload: { filter: "active", offset, limit: 100, query: null } });
          if (this.#disposed) return;
          for (const session of page.payload.sessions) {
            if (session.is_pinned && session.role !== "controller") {
              const key = JSON.stringify([null, session.session_id]);
              if (!pins.has(key)) pins.set(key, pinReference(session, null, "本机"));
            }
          }
          offset += page.payload.sessions.length;
          if (!page.payload.has_more || !page.payload.sessions.length || page.payload.sessions.some((session) => !session.is_pinned)) break;
        }
      }
      await this.#write({ imported: true, items: [...pins.values()] });
    } catch (failure) {
      runInAction(() => { this.error = `置顶导入失败，可重试：${String(failure)}`; });
    } finally { runInAction(() => { this.busy = false; }); }
  }

  async #commit(value: DesktopPins): Promise<boolean> {
    runInAction(() => { this.busy = true; this.error = null; });
    try { await this.#write(value); return !this.#disposed; }
    catch (failure) { runInAction(() => { this.error = `置顶保存失败，请重试：${String(failure)}`; }); return false; }
    finally { runInAction(() => { this.busy = false; }); }
  }

  async #write(value: DesktopPins): Promise<void> {
    if (value.items.length > 2048) throw new Error("置顶数量超出上限。");
    await this.save(value);
    if (!this.#disposed) runInAction(() => { this.value = value; });
  }

  dispose(): void { this.#disposed = true; }
}

export function pinReference(session: SessionSummary, device: string | null, device_name: string): DesktopPin {
  return { device, session_id: session.session_id, title: session.title, device_name };
}
