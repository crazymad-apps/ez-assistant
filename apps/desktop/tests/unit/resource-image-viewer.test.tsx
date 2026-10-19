import { createRef } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ReactZoomPanPinchRef } from "react-zoom-pan-pinch";
import { ResourceImageViewer } from "../../src/features/resource-workspace/ResourceImageViewer";
import type { ResourceViewState } from "../../src/features/resource-workspace/resourceViewState";

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

beforeEach(() => {
  vi.stubGlobal("Image", class {
    naturalWidth = 1600;
    naturalHeight = 1000;
    onload: (() => void) | null = null;
    set src(_value: string) { queueMicrotask(() => this.onload?.()); }
  });
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(600);
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(400);
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (this: HTMLElement) {
    return this.className.includes("transform-component") ? 1600 : 600;
  });
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (this: HTMLElement) {
    return this.className.includes("transform-component") ? 1000 : 400;
  });
});

it("uses the library's held-button drag and keeps its transform when the cached page is inactive", async () => {
  const view_state: NonNullable<ResourceViewState["preview"]> = {
    scroll_top: 0, scroll_left: 0, word_wrap: false, editor: null,
    image: { scale: 1, position_x: -200, position_y: -100 },
  };
  const ref = createRef<ReactZoomPanPinchRef>();
  const props = { alt: "test.png", data_url: "data:image/png;base64,fixture", media_type: "image/png", view_state, ref };
  const { rerender } = render(<ResourceImageViewer {...props} active />);
  const image = await screen.findByRole("img", { name: "test.png" });
  const drag = () => {
    fireEvent.mouseDown(image, { button: 0, buttons: 1, clientX: 300, clientY: 200 });
    fireEvent.mouseMove(window, { buttons: 1, clientX: 200, clientY: 150 });
    fireEvent.mouseUp(window, { button: 0, buttons: 0 });
  };
  drag();
  expect(view_state.image).toEqual({ scale: 1, position_x: -300, position_y: -150 });
  expect(image.parentElement?.style.transform).toBe("translate(-300px, -150px) scale(1)");
  rerender(<ResourceImageViewer {...props} active={false} />);
  drag();
  expect(view_state.image).toEqual({ scale: 1, position_x: -300, position_y: -150 });
  rerender(<ResourceImageViewer {...props} active />);
  expect(image.parentElement?.style.transform).toBe("translate(-300px, -150px) scale(1)");
});

it("keeps smaller images centered while zooming and dragging after fitting to the viewport", async () => {
  const view_state: NonNullable<ResourceViewState["preview"]> = {
    scroll_top: 0, scroll_left: 0, word_wrap: false, editor: null,
  };
  const ref = createRef<ReactZoomPanPinchRef>();
  render(<ResourceImageViewer active alt="test.png" data_url="data:image/png;base64,fixture"
    media_type="image/png" view_state={view_state} ref={ref} />);
  const image = await screen.findByRole("img", { name: "test.png" });
  const expectCentered = () => {
    const state = ref.current!.state;
    expect(state.positionX).toBeCloseTo((600 - 1600 * state.scale) / 2);
    expect(state.positionY).toBeCloseTo((400 - 1000 * state.scale) / 2);
  };

  for (let step = 0; step < 8; step += 1) {
    fireEvent.click(screen.getByRole("button", { name: "缩小图片" }));
    expectCentered();
  }
  fireEvent.click(screen.getByRole("button", { name: "适应窗口" }));
  expectCentered();
  fireEvent.mouseDown(image, { button: 0, buttons: 1, clientX: 300, clientY: 200 });
  fireEvent.mouseMove(window, { buttons: 1, clientX: 250, clientY: 150 });
  fireEvent.mouseUp(window, { button: 0, buttons: 0 });
  expectCentered();
  fireEvent.click(screen.getByRole("button", { name: "放大图片" }));
  fireEvent.click(screen.getByRole("button", { name: "缩小图片" }));
  expectCentered();
  fireEvent.wheel(image, { deltaY: 100, clientX: 100, clientY: 100 });
  expectCentered();
});
