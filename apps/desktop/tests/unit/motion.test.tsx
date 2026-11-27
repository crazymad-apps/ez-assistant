import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useRef, useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AppMessageProvider, useAppMessage } from "../../src/components/AppMessage";
import {
  gsap,
  prefersReducedMotion,
  runMotion,
  useFlipIndicator,
} from "../../src/components/Motion";
import { WorkspaceTransition } from "../../src/features/runtime-access/WorkspaceTransition";

const original_match_media = window.matchMedia;

function installMatchMedia(reduced: boolean): void {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: (query: string): MediaQueryList => {
      let matches = false;
      if (query.includes(": reduce")) matches = reduced;
      else if (query.includes(": no-preference")) matches = !reduced;
      return {
        addEventListener: vi.fn(),
        addListener: vi.fn(),
        dispatchEvent: vi.fn(),
        matches,
        media: query,
        onchange: null,
        removeEventListener: vi.fn(),
        removeListener: vi.fn(),
      };
    },
    writable: true,
  });
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: original_match_media,
    writable: true,
  });
});

describe("Motion helpers", () => {
  it("respects prefers-reduced-motion and degrades safely without matchMedia", () => {
    expect(prefersReducedMotion()).toBe(true);

    installMatchMedia(false);
    expect(prefersReducedMotion()).toBe(false);

    let received: boolean | null = null;
    const dispose = runMotion((reduced) => {
      received = reduced;
    });
    expect(received).toBe(false);
    if (typeof dispose === "function") dispose();

    installMatchMedia(true);
    expect(prefersReducedMotion()).toBe(true);

    let reduced_received: boolean | null = null;
    const dispose_reduced = runMotion((reduced) => {
      reduced_received = reduced;
    });
    expect(reduced_received).toBe(true);
    if (typeof dispose_reduced === "function") dispose_reduced();
  });

  it("positions and updates the Flip indicator when segmented tabs switch", () => {
    installMatchMedia(false);

    function SegmentedFixture() {
      const [active, setActive] = useState("first");
      const container_ref = useRef<HTMLDivElement>(null);
      const indicator_ref = useRef<HTMLSpanElement>(null);
      useFlipIndicator({
        active_key: active,
        active_selector: 'button[aria-pressed="true"]',
        container_ref,
        indicator_ref,
      });

      const attachBox = (left: number, width: number) => (node: HTMLButtonElement | null) => {
        if (!node) return;
        Object.defineProperties(node, {
          offsetHeight: { configurable: true, value: 32 },
          offsetLeft: { configurable: true, value: left },
          offsetTop: { configurable: true, value: 4 },
          offsetWidth: { configurable: true, value: width },
        });
      };

      return (
        <div data-testid="segmented" ref={container_ref}>
          <span data-testid="indicator" ref={indicator_ref} />
          <button aria-pressed={active === "first"} onClick={() => setActive("first")} ref={attachBox(4, 100)} type="button">一</button>
          <button aria-pressed={active === "second"} onClick={() => setActive("second")} ref={attachBox(108, 120)} type="button">二</button>
        </div>
      );
    }

    render(<SegmentedFixture />);
    const container = screen.getByTestId("segmented");
    const indicator = screen.getByTestId("indicator");
    expect(container.dataset.indicatorReady).toBe("true");
    expect(indicator.style.left).toBe("4px");
    expect(indicator.style.width).toBe("100px");

    fireEvent.click(screen.getByRole("button", { name: "二" }));
    expect(indicator.style.left).toBe("108px");
    expect(indicator.style.width).toBe("120px");
  });
});

describe("WorkspaceTransition GSAP flow", () => {
  it("plays the GSAP transition when motion is allowed, then focuses the workspace", () => {
    installMatchMedia(false);
    const page = render(
      <WorkspaceTransition entry={<button type="button">连接</button>} ready={false}>
        <textarea aria-label="输入消息" />
      </WorkspaceTransition>,
    );

    const root = page.container.firstElementChild as HTMLElement;
    expect(root.dataset.workspaceTransition).toBe("waiting");

    page.rerender(
      <WorkspaceTransition entry={<button type="button">连接</button>} ready>
        <textarea aria-label="输入消息" />
      </WorkspaceTransition>,
    );

    expect(root.dataset.workspaceTransition).toBe("transitioning");
    const workspace_layer = root.firstElementChild as HTMLElement;
    expect(workspace_layer).toHaveAttribute("inert");
    expect(page.container.querySelector('[aria-label="输入消息"]')).not.toHaveFocus();

    act(() => {
      gsap.globalTimeline.getChildren(true, true, true).forEach((child) => child.progress(1));
    });

    expect(root.dataset.workspaceTransition).toBe("complete");
    expect(workspace_layer).not.toHaveAttribute("inert");
    expect(screen.getByRole("textbox", { name: "输入消息" })).toHaveFocus();
    expect(screen.queryByRole("button", { name: "连接" })).toBeNull();
  });

  it("skips the transition when prefers-reduced-motion is enabled", () => {
    installMatchMedia(true);
    const page = render(
      <WorkspaceTransition entry={<button type="button">连接</button>} ready={false}>
        <textarea aria-label="输入消息" />
      </WorkspaceTransition>,
    );

    page.rerender(
      <WorkspaceTransition entry={<button type="button">连接</button>} ready>
        <textarea aria-label="输入消息" />
      </WorkspaceTransition>,
    );

    const root = page.container.firstElementChild as HTMLElement;
    expect(root.dataset.workspaceTransition).toBe("complete");
    expect(screen.getByRole("textbox", { name: "输入消息" })).toHaveFocus();
  });
});

describe("AppMessage GSAP lifecycle", () => {
  it("removes toast on schedule even when GSAP motion is enabled", () => {
    installMatchMedia(false);
    vi.useFakeTimers();

    function Trigger() {
      const show = useAppMessage();
      return <button onClick={() => show("已保存")} type="button">提示</button>;
    }

    render(
      <AppMessageProvider>
        <Trigger />
      </AppMessageProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "提示" }));
    expect(screen.getByRole("status")).toHaveTextContent("已保存");

    act(() => vi.advanceTimersByTime(3400));
    expect(screen.getByRole("status")).toBeInTheDocument();

    act(() => vi.advanceTimersByTime(100));
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });
});
