import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceSummary } from "@ez-assistant/protocol";
import { RemovedWorkspacesDialog } from "../../src/features/workspaces/RemovedWorkspacesDialog";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("RemovedWorkspacesDialog", () => {
  it("lists removed identities and restores the selected original ID", async () => {
    const store = new RootStore();
    const workspaces = [{
      workspace_id: "original-id", label: "同名项目", user_directory: null,
      additional_directories: [], agent_directory: "/private/original-id", lifecycle: "removed",
      created_at_ms: 1, updated_at_ms: 2, removed_at_ms: 3,
    }] as WorkspaceSummary[];
    vi.spyOn(store, "listRemovedWorkspaces").mockResolvedValue(workspaces);
    const restore = vi.spyOn(store, "restoreWorkspace").mockResolvedValue(true);
    const close = vi.fn();
    render(<RootStoreProvider store={store}><RemovedWorkspacesDialog on_close={close} open /></RootStoreProvider>);
    expect(await screen.findByText("同名项目")).toBeVisible();
    expect(screen.getByText("仅使用私有目录")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "恢复" }));
    await waitFor(() => expect(restore).toHaveBeenCalledWith("original-id"));
    await waitFor(() => expect(close).toHaveBeenCalled());
  });
});
