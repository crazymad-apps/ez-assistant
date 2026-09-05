import { createRef } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { ReactZoomPanPinchRef } from "react-zoom-pan-pinch";
import { ResourceImageViewer } from "../../src/features/resource-workspace/ResourceImageViewer";
import type { ResourceViewState } from "../../src/features/resource-workspace/resourceViewState";

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

it("uses the library's held-button drag and keeps its transform when the cached page is inactive", async () => {
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
