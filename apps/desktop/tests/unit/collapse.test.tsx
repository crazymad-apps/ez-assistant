import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Collapse } from "../../src/components/Collapse";
import { PresenceBoundary, usePresenceBoundary } from "../../src/components/Presence";

beforeEach(() => vi.useFakeTimers());
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

function renderContent(open: boolean) {
  return <StrictMode><Collapse open={open}><button>展开内容</button></Collapse></StrictMode>;
}

function contentContainer() {
  return screen.getByText("展开内容").closest("[data-presence]");
}

describe("Collapse", () => {
  it("renders initially open content immediately, including remounts and StrictMode effects", () => {
    const view = render(renderContent(true));
    expect(contentContainer()).toHaveAttribute("data-presence", "entered");
    expect(screen.getByRole("button")).toBeVisible();
    act(() => vi.advanceTimersByTime(200));
    expect(contentContainer()).toHaveAttribute("data-presence", "entered");
    view.unmount();
    render(renderContent(true));
    expect(contentContainer()).toHaveAttribute("data-presence", "entered");
  });

  it("still animates later opens and closes, including reversing an in-flight close", () => {
    const view = render(renderContent(false));
    expect(screen.queryByText("展开内容")).not.toBeInTheDocument();
    view.rerender(renderContent(true));
    expect(contentContainer()).toHaveAttribute("data-presence", "entering");
    act(() => vi.advanceTimersToNextFrame());
    expect(contentContainer()).toHaveAttribute("data-presence", "entered");
    view.rerender(renderContent(false));
    expect(contentContainer()).toHaveAttribute("data-presence", "exiting");
    expect(contentContainer()).toHaveAttribute("inert");
    view.rerender(renderContent(true));
    act(() => vi.advanceTimersByTime(200));
    expect(contentContainer()).toHaveAttribute("data-presence", "entered");
    expect(contentContainer()).not.toHaveAttribute("inert");
    view.rerender(renderContent(false));
    fireEvent.transitionEnd(contentContainer()!);
    expect(screen.queryByText("展开内容")).not.toBeInTheDocument();
  });

  it("reclaims initially open content when exit transitionend is unavailable", () => {
    const view = render(renderContent(true));
    view.rerender(renderContent(false));
    act(() => vi.advanceTimersByTime(160));
    expect(screen.queryByText("展开内容")).not.toBeInTheDocument();
  });

  it("preserves initial entry animations for other presence consumers", () => {
    function OverlayContent() {
      const presence = usePresenceBoundary();
      return <div data-presence={presence?.state}>浮层内容</div>;
    }
    render(<StrictMode><PresenceBoundary present><OverlayContent /></PresenceBoundary></StrictMode>);
    expect(screen.getByText("浮层内容")).toHaveAttribute("data-presence", "entering");
    act(() => vi.advanceTimersToNextFrame());
    expect(screen.getByText("浮层内容")).toHaveAttribute("data-presence", "entered");
  });
});
