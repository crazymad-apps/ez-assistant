import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ToolDetailView } from "../../src/features/conversation/ToolDetailDialog";
import { ToolDetailDialog } from "../../src/features/conversation/ToolDetailDialog";

const nativeMocks = vi.hoisted(() => ({
  previewToolFile: vi.fn(),
}));

vi.mock("../../src/native-bridge/nativeResource", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../src/native-bridge/nativeResource")>(),
  previewToolFile: nativeMocks.previewToolFile,
}));

afterEach(() => {
  cleanup();
  nativeMocks.previewToolFile.mockReset();
});

describe("ToolDetailDialog", () => {
  it("renders remote MCP identity and full arguments without a gateway request payload", () => {
    const arguments_json = JSON.stringify({ script: "完整参数".repeat(2000) });
    render(<ToolDetailDialog detail={detailView({
      tool_name: "call_mcp_tool", request_json: null,
      input: { type: "mcp", identity: { server_key: "blender", server_display_name: "Blender", tool_name: "execute_code" }, arguments_json },
    })} error={null} is_loading={false} on_close={vi.fn()} />);
    expect(screen.getByRole("heading", { name: "Blender (blender) / execute_code" })).toBeVisible();
    expect(screen.getByText(arguments_json)).toHaveTextContent(arguments_json);
    expect(screen.queryByRole("heading", { name: "call_mcp_tool" })).not.toBeInTheDocument();
  });

  it("uses formatted code blocks for JSON request and result payloads", () => {
    render(<ToolDetailDialog
      detail={detailView({
        request_json: "{\n  \"query\": \"进度\"\n}",
        result_json: "{\n  \"count\": 2\n}",
      })}
      error={null}
      is_loading={false}
      on_close={vi.fn()}
    />);

    const blocks = document.querySelectorAll("pre");
    expect(blocks).toHaveLength(2);
    expect(blocks[0]).toHaveTextContent('"query": "进度"');
    expect(blocks[1]).toHaveTextContent('"count": 2');
  });

  it("keeps Recall results readable and exposes their navigation target", () => {
    const on_navigate = vi.fn();
    render(<ToolDetailDialog
      detail={detailView({
        result_json: "{\"items\":[]}",
        recall: {
          items: [{
            content: "来源消息正文",
            role: "user",
            created_at_ms: 1,
            navigation: {
              owner: { type: "main_session", session_id: "source-session" },
              message_id: "source-message",
              lifecycle: "active",
            },
          }],
          failures: [],
          truncated: false,
        },
      })}
      error={null}
      is_loading={false}
      on_close={vi.fn()}
      on_recall_navigate={on_navigate}
    />);

    expect(screen.getByText("来源消息正文")).toBeVisible();
    expect(screen.queryByText('{"items":[]}')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "打开来源会话" }));
    expect(on_navigate).toHaveBeenCalledWith({
      owner: { type: "main_session", session_id: "source-session" },
      message_id: "source-message",
      lifecycle: "active",
    });
  });

  it("opens a read_image result directly as the primary image without exposing native file actions", async () => {
    const owner = {
      type: "child_task" as const,
      session_id: "session-image",
      child_task_id: "child-image",
    };
    nativeMocks.previewToolFile.mockResolvedValue({
      kind: "image",
      media_type: "image/jpeg",
      size_bytes: 128,
      text: null,
      data_url: "data:image/jpeg;base64,dGVzdA==",
    });
    render(<ToolDetailDialog
      detail={detailView({
        tool_name: "read_image",
        owner,
        message_id: "message-image",
        input: { type: "file", operation: "read_image", path: "/workspace/reference.png" },
        files: [{
          resource_ref_id: "tool-image-call-0",
          origin: "session_tool_image",
          display_name: `${"a".repeat(64)}.png`,
          display_path: null,
          size_bytes: null,
          media_type: "image/png",
          state: "available",
        }],
      })}
      error={null}
      is_loading={false}
      on_close={vi.fn()}
    />);

    await waitFor(() => expect(nativeMocks.previewToolFile).toHaveBeenCalledWith(
      owner,
      "message-image",
      "tool-image-call-0",
    ));
    expect(await screen.findByRole("img")).toBeVisible();
    expect(screen.getByText("/workspace/reference.png")).toBeVisible();
    expect(screen.queryByText("请求参数")).not.toBeInTheDocument();
    expect(screen.queryByText("执行结果")).not.toBeInTheDocument();
    expect(screen.queryByText("文件预览")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "系统打开" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "在目录中打开" })).not.toBeInTheDocument();
  });

  it("keeps a missing tool image as a local preview failure", async () => {
    nativeMocks.previewToolFile.mockRejectedValue(new Error("图片已不可用。"));
    render(<ToolDetailDialog
      detail={detailView({
        tool_name: "read_image",
        owner: { type: "main_session", session_id: "session-image" },
        message_id: "message-image",
        input: { type: "file", operation: "read_image", path: "/workspace/missing.png" },
        files: [{
          resource_ref_id: "tool-image-call-0",
          origin: "session_tool_image",
          display_name: `${"b".repeat(64)}.png`,
          display_path: null,
          size_bytes: null,
          media_type: "image/png",
          state: "available",
        }],
      })}
      error={null}
      is_loading={false}
      on_close={vi.fn()}
    />);

    expect(await screen.findByText("图片已不可用。")).toBeVisible();
    expect(screen.queryByRole("button", { name: /预览|不可用/ })).not.toBeInTheDocument();
  });
});

function detailView(overrides: Partial<ToolDetailView>): ToolDetailView {
  return {
    tool_name: "recall_memory",
    status: "completed",
    input: { type: "unavailable" },
    request_json: null,
    result_summary: null,
    result_json: null,
    recall: null,
    stdout: null,
    stderr: null,
    error: null,
    files: [],
    output_truncated: false,
    historical_fields_missing: false,
    ...overrides,
  };
}
