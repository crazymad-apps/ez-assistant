import { THEME_PRESETS } from "../../src/styles/themePresets";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MarkdownContent } from "../../src/components/MarkdownContent";
import { invoke, isTauri } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({
  isTauri: vi.fn(() => false),
  invoke: vi.fn().mockResolvedValue(undefined),
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.mocked(isTauri).mockReturnValue(false);
});

describe("MarkdownContent external links", () => {
  it("leaves Web links to the browser in a new tab, including modified clicks", () => {
    render(<MarkdownContent theme={THEME_PRESETS.default_light} text="[外链](https://example.com/report)" />);
    const link = screen.getByRole("link", { name: "外链" });
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
    expect(fireEvent.click(link)).toBe(true);
    expect(fireEvent.click(link, { ctrlKey: true })).toBe(true);
    expect(fireEvent.click(link, { metaKey: true })).toBe(true);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("keeps Desktop links routed through the native system browser", () => {
    vi.mocked(isTauri).mockReturnValue(true);
    render(<MarkdownContent theme={THEME_PRESETS.default_light} text="[外链](https://example.com/report)" />);
    expect(fireEvent.click(screen.getByRole("link", { name: "外链" }))).toBe(false);
    expect(invoke).toHaveBeenCalledWith("open_external_http_url", { url: "https://example.com/report" });
  });
});

describe("MarkdownContent local resources", () => {
  it("keeps file URIs out of navigable DOM and reports the original encoded reference", () => {
    const open = vi.fn();
    const open_menu = vi.fn();
    const { container } = render(
      <MarkdownContent theme={THEME_PRESETS.default_light}
        on_local_resource_open={open}
        on_local_resource_context_menu={open_menu}
        text="[打开报告](file:///Users/example/%E6%8A%A5%E5%91%8A%20final.md)"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /打开报告/ }));
    expect(open).toHaveBeenCalledWith("file:///Users/example/%E6%8A%A5%E5%91%8A%20final.md");
    fireEvent.contextMenu(screen.getByRole("button", { name: /打开报告/ }), { clientX: 31, clientY: 42 });
    expect(open_menu).toHaveBeenCalledWith(
      "file:///Users/example/%E6%8A%A5%E5%91%8A%20final.md",
      { x: 31, y: 42 },
    );
    expect(container.querySelector("[href^='file:'], [src^='file:']")).toBeNull();
  });

  it("loads Windows file images through the resource callback and exposes retry failures", async () => {
    const load = vi.fn().mockRejectedValueOnce(new Error("路径不存在。")).mockResolvedValue("blob:windows-image");
    const revoke = vi.fn();
    vi.stubGlobal("URL", Object.assign(URL, { revokeObjectURL: revoke }));
    render(<MarkdownContent theme={THEME_PRESETS.default_light} text="![产物](file:///C:/project/image.png)" load_local_image={load} />);
    await screen.findByText(/路径不存在/);
    fireEvent.click(screen.getByRole("button", { name: "重试图片" }));
    await waitFor(() => expect(screen.getByRole("img", { name: "产物" })).toHaveAttribute("src", "blob:windows-image"));
    expect(load).toHaveBeenCalledWith("file:///C:/project/image.png");
    fireEvent.error(screen.getByRole("img", { name: "产物" }));
    expect(screen.getByText(/图片无法显示/)).toBeVisible();
    cleanup();
    expect(revoke).toHaveBeenCalledWith("blob:windows-image");
    vi.unstubAllGlobals();
  });

  it("does not activate relative links unless the file viewer explicitly enables them", () => {
    const open = vi.fn();
    const first = render(<MarkdownContent theme={THEME_PRESETS.default_light} on_local_resource_open={open} text="[同级文件](notes.md)" />);
    expect(screen.queryByRole("button", { name: /同级文件/ })).not.toBeInTheDocument();
    first.unmount();

    render(
      <MarkdownContent theme={THEME_PRESETS.default_light}
        allow_relative_local_resources
        on_local_resource_open={open}
        text="[同级文件](notes.md)"
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /同级文件/ }));
    expect(open).toHaveBeenCalledWith("notes.md");
  });

  it("does not turn an incomplete streaming destination into a file link", () => {
    const { container } = render(
      <MarkdownContent theme={THEME_PRESETS.default_light} is_streaming on_local_resource_open={vi.fn()} text="[报告](file:///Users/example/report" />,
    );
    expect(container.querySelector("[href^='file:'], [src^='file:']")).toBeNull();
    expect(screen.queryByRole("button", { name: "报告" })).not.toBeInTheDocument();
  });
});
