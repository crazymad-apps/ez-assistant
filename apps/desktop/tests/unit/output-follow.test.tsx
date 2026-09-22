import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useRef } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useOutputFollow } from "../../src/features/conversation/ConversationView/useOutputFollow";
import type { ConversationReadingPosition } from "../../src/stores/NavigationStore";

let resize: () => void;
let height = 1000;
let top = 0;
let saved: ConversationReadingPosition | undefined;
function Fixture({ owner = "main", nested = false }: { owner?: string; nested?: boolean }) {
  const node = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const follow = useOutputFollow({ scroll: node, content, identity: owner, active: true,
    restore: () => saved, save: (value) => { saved = value; } });
  return <div ref={(element) => {
    node.current = element;
    if (element) Object.defineProperties(element, {
      clientHeight: { configurable: true, get: () => 100 },
      scrollHeight: { configurable: true, get: () => height },
      scrollTop: { configurable: true, get: () => top, set: (value) => { top = Math.max(0, Math.min(value, height - 100)); } },
    });
  }} aria-label="scroll" tabIndex={0}>
    <div ref={content}>{nested && <div data-output-scroll="true" aria-label="inner" />}</div>
    <button onClick={follow.bottom}>bottom</button>
  </div>;
}
function setup() {
  height = 1000; top = 0; saved = undefined;
  vi.stubGlobal("ResizeObserver", class {
    constructor(callback: () => void) { resize = callback; }
    observe() {} disconnect() {}
  });
  return render(<Fixture />);
}
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe("output follow intent", () => {
  it("keeps following large layout growth and stops only for user reading", () => {
    setup();
    const node = screen.getByLabelText("scroll");
    expect(top).toBe(900);
    height = 1800;
    act(() => resize());
    expect(top).toBe(1700);
    fireEvent.wheel(node, { deltaY: -500 }); top = 600; fireEvent.scroll(node);
    height = 2500; act(() => resize());
    expect(top).toBe(600);
    fireEvent.click(screen.getByText("bottom"));
    expect(top).toBe(2400);
    height = 3000; act(() => resize());
    expect(top).toBe(2900);
  });
  it("resumes after deliberate keyboard return to the bottom", () => {
    setup(); const node = screen.getByLabelText("scroll");
    fireEvent.keyDown(node, { key: "PageUp" }); top = 500; fireEvent.scroll(node);
    height = 2000; act(() => resize()); expect(top).toBe(500);
    fireEvent.keyDown(node, { key: "End" }); top = 1900; fireEvent.scroll(node);
    height = 2200; act(() => resize()); expect(top).toBe(2100);
  });
  it("handles scrollbar dragging and touch reading", () => {
    setup(); const node = screen.getByLabelText("scroll");
    fireEvent.pointerDown(node); top = 200; fireEvent.scroll(node);
    height = 1200; act(() => resize()); expect(top).toBe(200);
    fireEvent.click(screen.getByText("bottom"));
    fireEvent.touchStart(node, { touches: [{ clientY: 100 }] });
    fireEvent.touchMove(node, { touches: [{ clientY: 250 }] });
    top = 400; fireEvent.scroll(node);
    height = 1500; act(() => resize()); expect(top).toBe(400);
  });
  it("does not let an inner reasoning scroll pause the outer list", () => {
    const view = setup(); view.rerender(<Fixture nested />);
    const inner = screen.getByLabelText("inner");
    Object.defineProperties(inner, { scrollTop: { value: 200, configurable: true },
      clientHeight: { value: 100 }, scrollHeight: { value: 500 } });
    fireEvent.wheel(inner, { deltaY: -100 });
    height = 1400; act(() => resize()); expect(top).toBe(1300);
    Object.defineProperty(inner, "scrollTop", { value: 0 });
    fireEvent.wheel(inner, { deltaY: -100 }); top = 1000;
    fireEvent.scroll(screen.getByLabelText("scroll"));
    height = 1800; act(() => resize()); expect(top).toBe(1000);
  });
  it("restores paused position after remount without inheriting another owner", () => {
    const view = setup(); const node = screen.getByLabelText("scroll");
    fireEvent.wheel(node, { deltaY: -400 }); top = 300; fireEvent.scroll(node);
    const main = saved;
    view.unmount(); height = 1800;
    render(<Fixture />); expect(top).toBe(300);
    cleanup(); saved = undefined; render(<Fixture owner="child" />); expect(top).toBe(1700);
    cleanup(); saved = main; render(<Fixture />); expect(top).toBe(300);
  });
  it("preserves the visible message across prepending plus simultaneous tail growth", () => {
    setup(); const node = screen.getByLabelText("scroll");
    const anchor = document.createElement("div"); anchor.dataset.messageId = "m"; node.firstElementChild!.append(anchor);
    let offset = 0;
    vi.spyOn(anchor, "getBoundingClientRect").mockImplementation(() => ({ top: offset - top, bottom: offset - top + 1000 } as DOMRect));
    fireEvent.wheel(node, { deltaY: -400 }); top = 300; fireEvent.scroll(node);
    offset = 700; height = 2100; act(() => resize());
    expect(top).toBe(1000); // Only prepend height, not total +1100 growth.
  });
});
