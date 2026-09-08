import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { runInAction } from "mobx";
import { useRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { BrowserController } from "../../src/features/resource-workspace/BrowserController";
import { useBrowserSurface } from "../../src/features/resource-workspace/ResourceBrowser/useBrowserSurface";
import { captureResourceBrowser, layoutResourceBrowser } from "../../src/native-bridge/resourceBrowser";

vi.mock("../../src/native-bridge/resourceBrowser", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../src/native-bridge/resourceBrowser")>(),
  layoutResourceBrowser: vi.fn().mockResolvedValue(undefined),
  captureResourceBrowser: vi.fn().mockResolvedValue(null),
}));

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["requestAnimationFrame", "cancelAnimationFrame", "setTimeout", "clearTimeout"] });
  vi.mocked(layoutResourceBrowser).mockReset().mockResolvedValue(undefined);
  vi.mocked(captureResourceBrowser).mockReset().mockResolvedValue({ url: "https://example.com/", image: "data:image/png;base64,preview" });
});
afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); });

describe("native browser surface and HTML overlays", () => {
  it("keeps a settled overlay visible when its content scrolls without a browser tab", async () => {
    render(<Surface overlay />);
    await flushLayout();
    vi.mocked(layoutResourceBrowser).mockClear();
    const pending_hide = deferred();
    vi.mocked(layoutResourceBrowser).mockReturnValueOnce(pending_hide.promise);

    fireEvent.scroll(screen.getByRole("dialog"));
    await flushLayout();

    expect(screen.getByTestId("overlays").style.visibility).not.toBe("hidden");
    expect(layoutResourceBrowser).not.toHaveBeenCalled();
    await act(async () => pending_hide.resolve());
  });

  it("hides the native page once without hiding the portal, then keeps the dialog visible across scroll and resize", async () => {
    const controller = browser();
    const view = render(<Surface controller={controller} />);
    await flushLayout();
    expect(layoutResourceBrowser).toHaveBeenLastCalledWith("native-browser", bounds);
    const pending_hide = deferred();
    vi.mocked(layoutResourceBrowser).mockReturnValueOnce(pending_hide.promise);

    view.rerender(<Surface controller={controller} overlay />);
    await flushLayout();
    expect(screen.getByTestId("overlays").style.visibility).not.toBe("hidden");
    expect(layoutResourceBrowser).toHaveBeenLastCalledWith(null, null);
    await act(async () => pending_hide.resolve());
    expect(screen.getByRole("dialog")).toBeVisible();
    vi.mocked(layoutResourceBrowser).mockClear();

    for (const target of [screen.getByRole("dialog"), window]) {
      fireEvent.scroll(target);
      await flushLayout();
    }
    fireEvent.resize(window);
    await flushLayout();
    expect(layoutResourceBrowser).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog")).toBeVisible();

    view.rerender(<Surface controller={controller} />);
    await flushLayout();
    expect(layoutResourceBrowser).toHaveBeenLastCalledWith("native-browser", bounds);
  });

  it("remeasures scrolling but reapplies a visible browser after native window resize or focus", async () => {
    render(<Surface controller={browser()} />);
    await flushLayout();
    vi.mocked(layoutResourceBrowser).mockClear();
    fireEvent.scroll(window);
    await flushLayout();
    expect(layoutResourceBrowser).not.toHaveBeenCalled();

    const viewport = screen.getByTestId("viewport");
    vi.spyOn(viewport, "getBoundingClientRect").mockReturnValue(new DOMRect(25, 80, 400, 300));
    fireEvent.scroll(window);
    await flushLayout();
    expect(layoutResourceBrowser).toHaveBeenLastCalledWith("native-browser", { ...bounds, y: 80 });

    for (const event of ["resize", "focus", "visibilitychange"]) {
      vi.mocked(layoutResourceBrowser).mockClear();
      fireEvent(event === "visibilitychange" ? document : window, new Event(event));
      await flushLayout();
      expect(layoutResourceBrowser).toHaveBeenCalledTimes(1);
      expect(layoutResourceBrowser).toHaveBeenLastCalledWith("native-browser", { ...bounds, y: 80 });
    }
  });

  it("follows an in-flight native show with hide without gating the HTML overlay", async () => {
    const controller = browser();
    const view = render(<Surface />);
    await flushLayout();
    const pending_show = deferred();
    const pending_hide = deferred();
    vi.mocked(layoutResourceBrowser)
      .mockReturnValueOnce(pending_show.promise).mockReturnValueOnce(pending_hide.promise);
    view.rerender(<Surface controller={controller} />);
    await flushLayout();
    view.rerender(<Surface controller={controller} overlay />);
    await flushLayout();
    expect(screen.getByTestId("overlays").style.visibility).not.toBe("hidden");

    await act(async () => pending_show.resolve());
    expect(layoutResourceBrowser).toHaveBeenLastCalledWith(null, null);
    expect(screen.getByTestId("overlays").style.visibility).not.toBe("hidden");
    await act(async () => pending_hide.resolve());
    expect(screen.getByRole("dialog")).toBeVisible();
  });

  it("keeps non-overlapping menus live and reacts to a menu moving into the page", async () => {
    const controller = browser();
    const view = render(<Surface controller={controller} floating="outside" />);
    await flushLayout();
    expect(layoutResourceBrowser).toHaveBeenLastCalledWith("native-browser", bounds);
    vi.mocked(layoutResourceBrowser).mockClear();
    view.rerender(<Surface controller={controller} floating="inside" />);
    await flushLayout();
    expect(layoutResourceBrowser).toHaveBeenLastCalledWith(null, null);
  });

  it("ignores unregistered portal children, unpositioned and hidden regions", async () => {
    const controller = browser();
    const view = render(<Surface controller={controller} />);
    await flushLayout();
    vi.mocked(layoutResourceBrowser).mockClear();
    screen.getByTestId("overlays").append(document.createElement("div"));
    await flushLayout();
    expect(layoutResourceBrowser).not.toHaveBeenCalled();
    view.rerender(<Surface controller={controller} floating="inside" ready={false} />);
    await flushLayout();
    expect(layoutResourceBrowser).not.toHaveBeenCalled();
    view.rerender(<Surface controller={controller} floating="inside" hidden />);
    await flushLayout();
    expect(layoutResourceBrowser).not.toHaveBeenCalled();
  });

  it("counts visual tooltip overlap even when it does not receive pointer events", async () => {
    render(<Surface controller={browser()} floating="inside" />);
    await flushLayout();
    expect(layoutResourceBrowser).toHaveBeenLastCalledWith(null, null);
    expect(screen.getByRole("tooltip")).toBeVisible();
  });

  it("retains one preview while obscured and stops sampling until the page is live again", async () => {
    const controller = browser();
    const view = render(<Surface controller={controller} />);
    await flushLayout();
    expect(controller.preview).toBe("data:image/png;base64,preview");
    expect(captureResourceBrowser).toHaveBeenCalledTimes(1);
    view.rerender(<Surface controller={controller} overlay />);
    await flushLayout();
    expect(controller.obscured).toBe(true);
    await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
    expect(captureResourceBrowser).toHaveBeenCalledTimes(1);
    expect(controller.preview).not.toBeNull();
    view.rerender(<Surface controller={controller} />);
    await flushLayout();
    expect(controller.obscured).toBe(false);
    expect(captureResourceBrowser).toHaveBeenCalledTimes(2);
    view.unmount();
    expect(controller.preview).toBeNull();
    await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
    expect(captureResourceBrowser).toHaveBeenCalledTimes(2);
  });
});

const bounds = { x: 25, y: 50, width: 400, height: 300 };

function Surface(props: {
  readonly controller?: BrowserController;
  readonly overlay?: boolean;
  readonly floating?: "inside" | "outside";
  readonly ready?: boolean;
  readonly hidden?: boolean;
}) {
  const viewports = useRef(new Map<string, HTMLDivElement>());
  const overlays = useRef<HTMLDivElement>(null);
  useBrowserSurface(props.controller, true, viewports, props.controller ? "browser" : null, overlays);
  return <>
    <div data-testid="viewport" ref={(node) => {
      if (node) {
        node.getBoundingClientRect = () => new DOMRect(bounds.x, bounds.y, bounds.width, bounds.height);
        viewports.current.set("browser", node);
      } else viewports.current.delete("browser");
    }} />
    <div data-testid="overlays" ref={overlays}>
      {props.overlay && <div data-overlay-region="modal" role="dialog" ref={(node) => {
        if (node) node.getBoundingClientRect = () => new DOMRect(0, 0, 1000, 800);
      }}>设置</div>}
      {props.floating && <div data-overlay-region="floating" data-position-ready={props.ready ?? true}
        role="tooltip" style={{ display: props.hidden ? "none" : "block", pointerEvents: "none", left: props.floating === "inside" ? 40 : 500 }}
        ref={(node) => { if (node) node.getBoundingClientRect = () => new DOMRect(props.floating === "inside" ? 40 : 500, 70, 100, 100); }}>提示</div>}
    </div>
  </>;
}

function browser(): BrowserController {
  const controller = new BrowserController(() => {}, () => {});
  runInAction(() => { controller.native_id = "native-browser"; controller.url = "https://example.com/"; });
  return controller;
}

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((complete) => { resolve = complete; });
  return { promise, resolve };
}

async function flushLayout(): Promise<void> {
  await act(async () => { await vi.advanceTimersByTimeAsync(20); });
}
