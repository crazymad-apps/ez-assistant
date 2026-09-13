import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DesktopWindowControls } from "../../src/features/desktop-lifecycle/DesktopWindowControls";
import * as bridge from "../../src/native-bridge/desktopLifecycle";

vi.mock("../../src/native-bridge/desktopLifecycle", () => ({
  getDesktopPlatform: vi.fn(), isDesktopWindowMaximized: vi.fn(),
  listenDesktopWindowMaximized: vi.fn(), minimizeDesktopWindow: vi.fn(),
  requestDesktopClose: vi.fn(), toggleMaximizeDesktopWindow: vi.fn(),
}));

describe("desktop window controls", () => {
  afterEach(cleanup);
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(bridge.getDesktopPlatform).mockResolvedValue("windows");
    vi.mocked(bridge.isDesktopWindowMaximized).mockResolvedValue(false);
    vi.mocked(bridge.listenDesktopWindowMaximized).mockResolvedValue(vi.fn());
    vi.mocked(bridge.toggleMaximizeDesktopWindow).mockResolvedValue(true);
  });

  it("uses native minimize, maximize and the existing close intent on Windows", async () => {
    render(<DesktopWindowControls />);
    fireEvent.click(await screen.findByRole("button", { name: "最小化窗口" }));
    expect(bridge.minimizeDesktopWindow).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "最大化窗口" }));
    await screen.findByRole("button", { name: "还原窗口" });
    expect(bridge.toggleMaximizeDesktopWindow).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "关闭窗口" }));
    expect(bridge.requestDesktopClose).toHaveBeenCalledOnce();
  });

  it("does not replace macOS native window buttons", async () => {
    vi.mocked(bridge.getDesktopPlatform).mockResolvedValue("macos");
    render(<DesktopWindowControls />);
    await waitFor(() => expect(bridge.getDesktopPlatform).toHaveBeenCalled());
    expect(screen.queryByRole("button", { name: "关闭窗口" })).not.toBeInTheDocument();
  });

  it("releases the native window listener when leaving the page", async () => {
    const dispose = vi.fn();
    vi.mocked(bridge.listenDesktopWindowMaximized).mockResolvedValue(dispose);
    const view = render(<DesktopWindowControls />);
    await waitFor(() => expect(bridge.listenDesktopWindowMaximized).toHaveBeenCalled());
    view.unmount();
    expect(dispose).toHaveBeenCalledOnce();
  });
});
