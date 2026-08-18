import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ToolDetailView } from "../../src/features/conversation/ToolDetailDialog";
import { ToolDetailDialog } from "../../src/features/conversation/ToolDetailDialog";

afterEach(cleanup);

describe("ToolDetailDialog", () => {
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
