import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WorkspaceTransition } from "../../src/features/runtime-access/WorkspaceTransition";

const playback = vi.hoisted(() => ({ play: vi.fn() }));
vi.mock("../../src/features/runtime-access/WorkspaceTransition/rocketPlayback", () => ({ playRocket: playback.play }));
let reduced = false;
let finish: () => void;
let signal: AbortSignal;
beforeEach(() => {
  reduced = false;
  vi.stubGlobal("matchMedia", () => ({ matches: reduced, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
  playback.play.mockImplementation((_canvas: HTMLCanvasElement, abort: AbortSignal) => {
    signal = abort;
    return new Promise<void>((resolve) => { finish = resolve; });
  });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.clearAllMocks(); });

function view(ready: boolean) {
  return <WorkspaceTransition ready={ready} connected={ready} entry={<button>连接</button>}>
    <textarea aria-label="输入消息" />
  </WorkspaceTransition>;
}

describe("first workspace transition", () => {
  it("waits for connection readiness, then releases the overlay and hands off focus once", async () => {
    const page = render(view(false));
    expect(screen.getByRole("button", { name: "连接" })).toBeVisible();
    expect(playback.play).not.toHaveBeenCalled();
    page.rerender(view(true));
    await waitFor(() => expect(playback.play).toHaveBeenCalledOnce());
    expect(screen.queryByRole("textbox", { name: "输入消息" })).toBeNull();
    await act(async () => { finish(); });
    expect(screen.getByRole("textbox", { name: "输入消息" })).toHaveFocus();
    expect(page.container.querySelector("canvas")).toBeNull();
    expect(signal.aborted).toBe(true);
    page.rerender(view(false)); page.rerender(view(true));
    expect(playback.play).toHaveBeenCalledOnce();
  });

  it("skips motion without loading the player when reduced motion is requested", async () => {
    reduced = true;
    const page = render(view(true));
    await waitFor(() => expect(screen.getByRole("textbox", { name: "输入消息" })).toHaveFocus());
    expect(playback.play).not.toHaveBeenCalled();
    expect(page.container.querySelector("canvas")).toBeNull();
  });

  it("makes the ready workspace usable if media decoding fails", async () => {
    playback.play.mockRejectedValueOnce(new Error("decode failed"));
    const page = render(view(true));
    await waitFor(() => expect(screen.getByRole("textbox", { name: "输入消息" })).toHaveFocus());
    expect(page.container.querySelector("canvas")).toBeNull();
  });

  it("cancels pending animation work when the view unmounts", async () => {
    const page = render(view(true));
    await waitFor(() => expect(playback.play).toHaveBeenCalledOnce());
    page.unmount();
    expect(signal.aborted).toBe(true);
    await act(async () => { finish(); });
  });

  it("settles on backgrounding and focuses the workspace when the composer is disabled", async () => {
    const page = render(<WorkspaceTransition ready connected entry={<button>连接</button>}>
      <textarea aria-label="输入消息" disabled />
    </WorkspaceTransition>);
    await waitFor(() => expect(playback.play).toHaveBeenCalledOnce());
    const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    await act(async () => { document.dispatchEvent(new Event("visibilitychange")); });
    expect(page.container.querySelector('[data-workspace-transition="complete"] > div')).toHaveFocus();
    expect(signal.aborted).toBe(true);
    hidden.mockRestore();
    await act(async () => { finish(); });
  });
});
