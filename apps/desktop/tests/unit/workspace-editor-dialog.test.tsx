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
  it("opens before choosing a directory and creates a private-directory workspace", async () => {
    const store = new RootStore();
    const choose = vi.spyOn(store, "chooseWorkspaceDirectory");
    store.addWorkspace();
    expect(choose).not.toHaveBeenCalled();
    const save = vi.spyOn(store, "saveWorkspaceEditor").mockResolvedValue(true);

    render(<RootStoreProvider store={store}><WorkspaceEditorDialog /></RootStoreProvider>);
    expect(screen.getByText(/仅使用私有目录/)).toBeVisible();
    fireEvent.change(screen.getByRole("textbox", { name: /项目名称/ }), { target: { value: "私有项目" } });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));

    await waitFor(() => expect(save).toHaveBeenCalledWith({
      label: "私有项目",
      primary_directory: null,
      additional_directories: [],
    }));
  });

  it("allows removing the final user directory and retains edits when save fails", async () => {
    const store = new RootStore();
    store.projection.applyApplicationSnapshot({
      observed_sequence: 1,
      value: {
        runtime_lifecycle: "running",
        active_sessions_next_offset: null,
        archived_sessions_next_offset: null,
        configuration: { state: "ready" },
        providers: [], model_settings: { default_model: null, vision_model: null },
        workspaces: [{
          workspace_id: "workspace-1", label: "项目", user_directory: "/workspace/project",
          additional_directories: [], lifecycle: "active",
        }],
        active_sessions: [], archived_sessions: [],
      },
    } as unknown as Parameters<RootStore["projection"]["applyApplicationSnapshot"]>[0]);
    store.openWorkspaceEditor("workspace-1");
    const save = vi.spyOn(store, "saveWorkspaceEditor").mockResolvedValue(false);
    render(<RootStoreProvider store={store}><WorkspaceEditorDialog /></RootStoreProvider>);
    fireEvent.click(screen.getByRole("button", { name: "移除 project" }));
    expect(screen.getByText(/仅使用私有目录/)).toBeVisible();
    fireEvent.change(screen.getByRole("textbox", { name: /项目名称/ }), { target: { value: "新项目" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(save).toHaveBeenCalledWith({
      label: "新项目", primary_directory: null, additional_directories: [],
    }));
    expect(screen.getByRole("textbox", { name: /项目名称/ })).toHaveValue("新项目");
    expect(screen.getByRole("alert")).toHaveTextContent("保存项目失败");
  });

  it("edits the name and ordered directories as one full form", async () => {
    const store = new RootStore();
    store.projection.applyApplicationSnapshot({
      observed_sequence: 1,
      value: {
        runtime_lifecycle: "running",
        active_sessions_next_offset: null,
        archived_sessions_next_offset: null,
        configuration: { state: "ready" },
        providers: [], model_settings: { default_model: null, vision_model: null },
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
    fireEvent.change(screen.getByRole("textbox", { name: /项目名称/ }), {
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
        active_sessions_next_offset: null,
        archived_sessions_next_offset: null,
        configuration: { state: "ready" },
        providers: [], model_settings: { default_model: null, vision_model: null },
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
    fireEvent.change(screen.getByRole("textbox", { name: /项目名称/ }), {
      target: { value: "未保存名称" },
    });
    fireEvent.click(screen.getByRole("button", { name: "关闭项目编辑" }));

    expect(screen.getByRole("dialog", { name: "放弃项目修改" })).toBeVisible();
    expect(store.workspace_editor).not.toBeNull();
  });
});
