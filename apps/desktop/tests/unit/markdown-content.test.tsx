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

describe("MarkdownContent CJK emphasis", () => {
  it.each([false, true])("renders bold next to CJK punctuation with streaming=%s", (is_streaming) => {
    render(<MarkdownContent theme={THEME_PRESETS.default_light} is_streaming={is_streaming}
      text={"- **今日主线：**刻赤大桥\n\n`**今日主线：**刻赤大桥`"} />);

    const label = screen.getByText("今日主线：", { selector: '[data-streamdown="strong"]' });
    expect(label.closest("li")).toHaveTextContent("今日主线：刻赤大桥");
    expect(screen.getByText("**今日主线：**刻赤大桥", { selector: "code" })).toBeVisible();
  });
});

describe("MarkdownContent table columns", () => {
  it("keeps short labels and long content in separate sizing boxes in any column", () => {
    const note = "数量仍在复核，已提交采购计划。后续交付批次和预计交付时间将在复核完成后更新。";
    const description = "这一列用于展示完整说明，有较多文字，应该获得更多宽度并自然换行。";
    const text = [
      "| 说明 | 数量 | 编号 |",
      "| --- | --- | --- |",
      `| ${description} | **36** | A01 |`,
      `| ${description} | 45 | A02 |`,
      `| ${description} | 50 | A03 |`,
      `| ${description} | ${note} | A04 |`,
    ].join("\n");

    render(<MarkdownContent theme={THEME_PRESETS.default_light} text={text} />);

    const value = screen.getByRole("cell", { name: "36" });
    expect(value.firstElementChild?.firstElementChild).toHaveAttribute("data-streamdown", "strong");
    const note_content = screen.getByRole("cell", { name: note }).firstElementChild;
    const prose_content = screen.getAllByRole("cell", { name: description })[0]?.firstElementChild;
    expect(note_content).toHaveTextContent(note);
    expect(prose_content).toHaveTextContent(description);
    expect(note_content?.className).not.toBe(prose_content?.className);
    expect(screen.getByRole("cell", { name: "A01" }).firstElementChild).toHaveTextContent("A01");
  });
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
