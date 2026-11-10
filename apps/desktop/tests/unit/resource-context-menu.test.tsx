import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ResourceContextMenu } from "../../src/features/resource-workspace/ResourceContextMenu";

afterEach(() => {
  cleanup();
  document.querySelector("#overlay-root")?.remove();
  vi.restoreAllMocks();
});

it("keeps resource actions outside contained messages and closes on conversation scroll", () => {
  const overlay = document.createElement("div");
  overlay.id = "overlay-root";
  document.body.append(overlay);
  const close = vi.fn();
  const select = vi.fn();
  const { container } = render(<section style={{ contentVisibility: "auto" }}>
    <ResourceContextMenu location={{ x: 100, y: 150 }} on_close={close}
      items={[{ label: "下载文件", on_select: select }]} />
  </section>);
  const menu = screen.getByRole("menu");
  expect(overlay).toContainElement(menu);
  expect(container).not.toContainElement(menu);
  expect(menu).toHaveAttribute("data-overlay-region", "floating");
  expect(screen.getByRole("menuitem")).toHaveFocus();
  fireEvent.scroll(menu);
  expect(close).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("menuitem"));
  expect(select).toHaveBeenCalledOnce();
  close.mockClear();
  fireEvent.scroll(container);
  expect(close).toHaveBeenCalledOnce();
});

it("clamps the measured menu at viewport edges and restores focus without scrolling", () => {
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(
    { width: 260, height: 210 } as DOMRect,
  );
  const opener = document.createElement("button");
  document.body.append(opener);
  opener.focus();
  const focus = vi.spyOn(opener, "focus");
  const close = vi.fn();
  const view = render(<ResourceContextMenu
    location={{ x: innerWidth - 1, y: innerHeight - 1 }} on_close={close}
    items={[{ label: "在资源栏打开", on_select: vi.fn() }]} />);
  expect(screen.getByRole("menu")).toHaveStyle({
    left: `${innerWidth - 268}px`, top: `${innerHeight - 218}px`,
  });
  fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
  expect(close).toHaveBeenCalledOnce();
  view.unmount();
  expect(focus).toHaveBeenCalledWith({ preventScroll: true });
  opener.remove();
});
