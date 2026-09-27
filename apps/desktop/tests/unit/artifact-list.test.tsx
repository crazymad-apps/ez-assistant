import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SessionArtifact, SessionId } from "@ez-assistant/protocol";
import { ArtifactList } from "../../src/features/resource-workspace/ArtifactList";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";

afterEach(cleanup);
const session = "session-artifacts" as SessionId;
const artifact: SessionArtifact = { title: "最终图纸", source_run_id: "run-1" as SessionArtifact["source_run_id"], locator: { root: { type: "session_private" }, relative_path: "final.svg" } };
function setup() {
  const store = new RootStore();
  const stat = vi.spyOn(store.files, "statSessionResourceFile").mockResolvedValue({ size_bytes: 12 });
  const open = vi.spyOn(store.resource_workspace, "openSessionResource").mockImplementation(() => {});
  const download = vi.spyOn(store.files, "downloadSessionFile").mockResolvedValue(undefined);
  const error = vi.spyOn(store, "showInteractionError").mockImplementation(() => {});
  const ui = render(<RootStoreProvider store={store}><ArtifactList session_id={session} artifacts={[artifact]} /></RootStoreProvider>);
  const refresh = (refresh_key: number) => ui.rerender(<RootStoreProvider store={store}><ArtifactList session_id={session} artifacts={[artifact]} refresh_key={refresh_key} /></RootStoreProvider>);
  return { stat, open, download, error, refresh, ...ui };
}

describe("产物", () => {
  it("uses existing preview/download with a fresh check", async () => {
    const { stat, open, download } = setup();
    const button = screen.getByRole("button", { name: "最终图纸" });
    expect(button).toBeDisabled();
    await waitFor(() => expect(button).toBeEnabled());
    fireEvent.click(button);
    await waitFor(() => expect(open).toHaveBeenCalledWith(`session:${session}`, session, artifact.locator, "final.svg"));
    fireEvent.click(screen.getByRole("button", { name: "下载 最终图纸" }));
    await waitFor(() => expect(download).toHaveBeenCalledWith(session, artifact.locator, "final.svg"));
    expect(stat).toHaveBeenCalledTimes(3);
  });
  it("retains unavailable items and recovers after refresh", async () => {
    const { stat, open, refresh } = setup();
    stat.mockRejectedValue(new Error("路径不存在"));
    refresh(1);
    expect(await screen.findByText("文件不可用：路径不存在")).toBeVisible();
    expect(screen.getByRole("button", { name: "下载 最终图纸" })).toBeDisabled();
    expect(open).not.toHaveBeenCalled();
    stat.mockResolvedValue({ size_bytes: 12 });
    refresh(2);
    await waitFor(() => expect(screen.getByRole("button", { name: "最终图纸" })).toBeEnabled());
  });
  it("rejects a file removed after the initial check", async () => {
    const { stat, open, error } = setup();
    await waitFor(() => expect(screen.getByRole("button", { name: "最终图纸" })).toBeEnabled());
    stat.mockRejectedValue(new Error("文件已删除"));
    fireEvent.click(screen.getByRole("button", { name: "最终图纸" }));
    await waitFor(() => expect(error).toHaveBeenCalledWith("文件已删除"));
    expect(open).not.toHaveBeenCalled();
    expect(await screen.findByText("文件不可用：文件已删除")).toBeVisible();
  });
  it("drops an open operation after the owning view unmounts", async () => {
    const { stat, open, unmount } = setup();
    await waitFor(() => expect(screen.getByRole("button", { name: "最终图纸" })).toBeEnabled());
    let resolve!: (value: { size_bytes: number }) => void;
    stat.mockReturnValue(new Promise((done) => { resolve = done; }));
    fireEvent.click(screen.getByRole("button", { name: "最终图纸" }));
    unmount();
    await act(async () => { resolve({ size_bytes: 12 }); });
    expect(open).not.toHaveBeenCalled();
  });
});
