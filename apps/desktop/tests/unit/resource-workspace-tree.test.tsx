import type { ResourceViewState } from "../../src/features/resource-workspace/resourceViewState";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SessionResourceTree } from "../../src/features/resource-workspace/SessionResourceTree";
import { listSessionResourceFiles } from "../../src/native-bridge/nativeResource";

vi.mock("../../src/native-bridge/nativeResource", async (import_original) => {
  const original = await import_original<typeof import("../../src/native-bridge/nativeResource")>();
  return { ...original, listSessionResourceFiles: vi.fn() };
});

afterEach(() => {
  cleanup();
  vi.mocked(listSessionResourceFiles).mockReset();
});

describe("SessionResourceTree", () => {
  it("loads one directory level and exposes deterministic boundary states", async () => {
    const on_open_file = vi.fn();
    vi.mocked(listSessionResourceFiles).mockResolvedValue({
      entries: [
        {
          locator: { root: { type: "workspace_primary" }, relative_path: "src" },
          display_name: "src",
          kind: "directory",
          state: "available",
          is_symbolic_link: false,
          is_hidden: false,
          is_generated: false,
        },
        {
          locator: { root: { type: "workspace_primary" }, relative_path: "escape" },
          display_name: "escape",
          kind: "file",
          state: "outside_root",
          is_symbolic_link: true,
          is_hidden: false,
          is_generated: false,
        },
        {
          locator: { root: { type: "workspace_primary" }, relative_path: "README.md" },
          display_name: "README.md",
          kind: "file",
          state: "available",
          is_symbolic_link: false,
          is_hidden: false,
          is_generated: false,
          size_bytes: 1024,
        },
      ],
      truncated: true,
    });
    render(<SessionResourceTree
      focus_locator={null}
      roots={[{
        id: "primary",
        label: "project",
        detail: "主目录",
        locator: { root: { type: "workspace_primary" }, relative_path: "" },
      }]}
      on_open_file={on_open_file}
      session_id="session-1"
    />);

    fireEvent.click(screen.getByRole("button", { name: /project/ }));
    await waitFor(() => expect(screen.getByText("README.md")).toBeVisible());
    fireEvent.doubleClick(screen.getByRole("button", { name: /README\.md/ }));
    expect(on_open_file).toHaveBeenCalledWith(expect.objectContaining({ display_name: "README.md" }));
    expect(screen.getByText("README.md").closest("[title]")).toBeNull();
    expect(screen.getByText("1.0 KB")).toBeVisible();
    expect(screen.getByText("目标位于当前根之外")).toBeVisible();
    expect(screen.getByText("目录内容过多，请使用 Finder 查看。")).toBeVisible();
    expect(listSessionResourceFiles).toHaveBeenCalledWith("session-1", {
      locator: { root: { type: "workspace_primary" }, relative_path: "" },
      include_hidden: false,
      include_generated: false,
    });
    const root_control = screen.getByText("project").closest("button");
    expect(root_control).not.toBeNull();
    fireEvent.click(root_control!);
    fireEvent.click(root_control!);
    expect(screen.getByText("README.md")).toBeVisible();
    expect(listSessionResourceFiles).toHaveBeenCalledTimes(1);
  });

  it("reloads expanded directories when hidden files are enabled", async () => {
    vi.mocked(listSessionResourceFiles).mockResolvedValue({ entries: [], truncated: false });
    render(<SessionResourceTree
      focus_locator={{ root: { type: "session_private" }, relative_path: "" }}
      roots={[{
        id: "private",
        label: "会话私有目录",
        detail: "当前会话",
        locator: { root: { type: "session_private" }, relative_path: "" },
      }]}
      session_id="session-1"
    />);

    await waitFor(() => expect(listSessionResourceFiles).toHaveBeenCalledTimes(1));
    expect(screen.queryByText("目录为空")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("checkbox", { name: "显示隐藏项" }));
    await waitFor(() => expect(listSessionResourceFiles).toHaveBeenLastCalledWith("session-1", {
      locator: { root: { type: "session_private" }, relative_path: "" },
      include_hidden: true,
      include_generated: false,
    }));
  });

  it("recursively expands available directories and collapses the tree", async () => {
    vi.mocked(listSessionResourceFiles).mockImplementation(async (_session_id, request) => {
      if (request.locator.relative_path === "") {
        return {
          entries: [{
            locator: { root: { type: "workspace_primary" }, relative_path: "src" },
            display_name: "src",
            kind: "directory",
            state: "available",
            is_symbolic_link: false,
            is_hidden: false,
            is_generated: false,
          }],
          truncated: false,
        };
      }
      if (request.locator.relative_path === "src") {
        return {
          entries: [{
            locator: { root: { type: "workspace_primary" }, relative_path: "src/components" },
            display_name: "components",
            kind: "directory",
            state: "available",
            is_symbolic_link: false,
            is_hidden: false,
            is_generated: false,
          }],
          truncated: false,
        };
      }
      return { entries: [], truncated: false };
    });
    render(<SessionResourceTree
      focus_locator={null}
      roots={[{
        id: "primary",
        label: "project",
        detail: "主目录",
        locator: { root: { type: "workspace_primary" }, relative_path: "" },
      }]}
      session_id="session-1"
    />);

    fireEvent.click(screen.getByRole("button", { name: "全部展开" }));
    await waitFor(() => expect(screen.getByText("components")).toBeVisible());
    expect(listSessionResourceFiles).toHaveBeenCalledTimes(3);

    fireEvent.click(screen.getByRole("button", { name: "全部收起" }));
    expect(screen.queryByText("src")).not.toBeInTheDocument();
  });

  it("opens every ancestor when a workspace location is focused", async () => {
    vi.mocked(listSessionResourceFiles).mockImplementation(async (_session_id, request) => {
      if (request.locator.relative_path === "") {
        return {
          entries: [{
            locator: { root: { type: "workspace_primary" }, relative_path: "src" },
            display_name: "src",
            kind: "directory",
            state: "available",
            is_symbolic_link: false,
            is_hidden: false,
            is_generated: false,
          }],
          truncated: false,
        };
      }
      if (request.locator.relative_path === "src") {
        return {
          entries: [{
            locator: { root: { type: "workspace_primary" }, relative_path: "src/components" },
            display_name: "components",
            kind: "directory",
            state: "available",
            is_symbolic_link: false,
            is_hidden: false,
            is_generated: false,
          }],
          truncated: false,
        };
      }
      return { entries: [], truncated: false };
    });
    render(<SessionResourceTree
      focus_locator={{ root: { type: "workspace_primary" }, relative_path: "src/components" }}
      roots={[{
        id: "primary",
        label: "project",
        detail: "主目录",
        locator: { root: { type: "workspace_primary" }, relative_path: "" },
      }]}
      session_id="session-1"
    />);

    await waitFor(() => expect(screen.getByRole("button", { name: "components" }).closest('[role="treeitem"]'))
      .toHaveAttribute("aria-expanded", "true"));
    expect(listSessionResourceFiles).toHaveBeenCalledTimes(3);
    expect(listSessionResourceFiles).toHaveBeenCalledWith("session-1", expect.objectContaining({
      locator: { root: { type: "workspace_primary" }, relative_path: "" },
    }));
    expect(listSessionResourceFiles).toHaveBeenCalledWith("session-1", expect.objectContaining({
      locator: { root: { type: "workspace_primary" }, relative_path: "src" },
    }));
    expect(listSessionResourceFiles).toHaveBeenCalledWith("session-1", expect.objectContaining({
      locator: { root: { type: "workspace_primary" }, relative_path: "src/components" },
    }));
  });

  it("shows draft roots without fabricating a session path", () => {
    render(<SessionResourceTree
      focus_locator={null}
      roots={[{ id: "draft", label: "project", detail: "主目录", locator: null }]}
      session_id={null}
    />);

    expect(screen.getByText("project")).toBeVisible();
    expect(screen.queryByRole("button", { name: /project/ })).not.toBeInTheDocument();
    expect(screen.getByText("创建会话后可浏览")).toBeVisible();
    expect(listSessionResourceFiles).not.toHaveBeenCalled();
  });
});

it("rebuilds expanded directories and filtering after eviction without replaying a consumed focus intent", async () => {
  vi.mocked(listSessionResourceFiles).mockResolvedValue({ entries: [], truncated: false });
  const root = { root: { type: "session_private" as const }, relative_path: "" };
  const state: ResourceViewState = {};
  const props = { view_state: state, session_id: "a", focus_locator: root,
    roots: [{ id: "private", label: "private", detail: "当前会话", locator: root }] };
  const first = render(<SessionResourceTree {...props} />);
  await waitFor(() => expect(listSessionResourceFiles).toHaveBeenCalledOnce());
  fireEvent.click(screen.getByRole("checkbox", { name: "显示隐藏项" }));
  await waitFor(() => expect(listSessionResourceFiles).toHaveBeenCalledTimes(2));
  first.unmount();
  const second = render(<SessionResourceTree {...props} />);
  await waitFor(() => expect(listSessionResourceFiles).toHaveBeenCalledTimes(3));
  expect(listSessionResourceFiles).toHaveBeenLastCalledWith("a", expect.objectContaining({ include_hidden: true }));
  expect(screen.getByRole("treeitem")).toHaveAttribute("aria-expanded", "true");
  fireEvent.click(screen.getByRole("button", { name: "全部收起" }));
  second.unmount();
  render(<SessionResourceTree {...props} />);
  expect(screen.getByRole("treeitem")).toHaveAttribute("aria-expanded", "false");
  expect(listSessionResourceFiles).toHaveBeenCalledTimes(3);
});
