import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { WorkspaceEditorDialog } from "../../src/features/workspaces/WorkspaceEditorDialog";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("WorkspaceEditorDialog", () => {
  it("edits the name and ordered directories as one full form", async () => {
    const store = new RootStore();
    store.projection.applyApplicationSnapshot({
      observed_sequence: 1,
      value: {
        runtime_lifecycle: "running",
        configuration: { state: "ready" },
        models: [],
        workspaces: [{
          workspace_id: "workspace-1",
          label: "旧名称",
          user_directory: "/workspace/primary",
          additional_directories: ["/workspace/docs"],
          lifecycle: "active",
        }],
        active_sessions: [],
        archived_sessions: [],
      },
    } as unknown as Parameters<RootStore["projection"]["applyApplicationSnapshot"]>[0]);
    store.openWorkspaceEditor("workspace-1");
    const save = vi.spyOn(store, "saveWorkspaceEditor").mockResolvedValue(true);

    render(
      <RootStoreProvider store={store}>
        <WorkspaceEditorDialog />
      </RootStoreProvider>,
    );

    expect(screen.queryByText("名称保存后立即用于全局展示", { exact: false })).not.toBeInTheDocument();
    fireEvent.change(screen.getByRole("textbox", { name: /工作空间名称/ }), {
      target: { value: "新名称" },
    });
    fireEvent.click(screen.getByRole("button", { name: "设为主目录" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(save).toHaveBeenCalledWith({
      label: "新名称",
      primary_directory: "/workspace/docs",
      additional_directories: ["/workspace/primary"],
    }));
  });

  it("uses a product dialog before discarding dirty edits", () => {
    const store = new RootStore();
    store.projection.applyApplicationSnapshot({
      observed_sequence: 1,
      value: {
        runtime_lifecycle: "running",
        configuration: { state: "ready" },
        models: [],
        workspaces: [{
          workspace_id: "workspace-1",
          label: "项目",
          user_directory: "/workspace/project",
          additional_directories: [],
          lifecycle: "active",
        }],
        active_sessions: [],
        archived_sessions: [],
      },
    } as unknown as Parameters<RootStore["projection"]["applyApplicationSnapshot"]>[0]);
    store.openWorkspaceEditor("workspace-1");

    render(
      <RootStoreProvider store={store}>
        <WorkspaceEditorDialog />
      </RootStoreProvider>,
    );
    fireEvent.change(screen.getByRole("textbox", { name: /工作空间名称/ }), {
      target: { value: "未保存名称" },
    });
    fireEvent.click(screen.getByRole("button", { name: "关闭工作空间编辑" }));

    expect(screen.getByRole("dialog", { name: "放弃工作空间修改" })).toBeVisible();
    expect(store.workspace_editor).not.toBeNull();
  });
});
