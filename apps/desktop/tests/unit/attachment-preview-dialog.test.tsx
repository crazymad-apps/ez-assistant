import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  NativeResourceFailure,
  openAttachmentInSystem,
  previewAttachment,
  revealAttachmentInDirectory,
} from "../../src/native-bridge/nativeResource";
import { AttachmentPreviewDialog } from "../../src/features/context-panel/AttachmentPreviewDialog";

vi.mock("../../src/native-bridge/nativeResource", async (import_original) => {
  const original = await import_original<typeof import("../../src/native-bridge/nativeResource")>();
  return {
    ...original,
    openAttachmentInSystem: vi.fn().mockResolvedValue(undefined),
    previewAttachment: vi.fn(),
    revealAttachmentInDirectory: vi.fn().mockResolvedValue(undefined),
  };
});

describe("AttachmentPreviewDialog", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("turns unsupported previews into a useful fallback and can reveal the file", async () => {
    vi.mocked(previewAttachment).mockRejectedValue(new NativeResourceFailure(
      "resource type is not previewable",
      "resource_not_previewable",
    ));
    const user = userEvent.setup();

    render(<AttachmentPreviewDialog
      attachment={{
        attachment_id: "attachment-1",
        session_id: "session-1",
        original_name: "南泉海事1.zyb",
        size_bytes: 6_300_000,
        agent_readable_path: "",
        state: "ready",
        created_at_ms: 1,
      }}
      on_close={vi.fn()}
    />);

    expect(await screen.findByText("此文件暂不支持应用内预览")).toBeVisible();
    expect(screen.queryByText("resource type is not previewable")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "在目录中打开" }));
    await waitFor(() => expect(revealAttachmentInDirectory).toHaveBeenCalledWith(
      "session-1",
      "attachment-1",
    ));
    expect(openAttachmentInSystem).not.toHaveBeenCalled();
  });
});
