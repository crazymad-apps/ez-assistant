import { describe, expect, it } from "vitest";

import type {
  AssistantMessageSnapshot,
  RuntimeHostCapabilities,
} from "../../src/generated/assistant-protocol";

describe("generated assistant protocol", () => {
  it("represents additive host capabilities", () => {
    const capabilities = {
      protocol_version: 1,
      runtime_version: "0.1.0",
      max_command_bytes: 1024,
      max_attachment_bytes: null,
      sse: true,
      streaming_upload: true,
      features: ["event_envelopes", "session_view"],
    } satisfies RuntimeHostCapabilities;

    expect(capabilities.features).toEqual(["event_envelopes", "session_view"]);
  });

  it("keeps assistant segments in runtime order", () => {
    const message = {
      message_id: "message-1",
      run_id: null,
      attempt: null,
      created_at_ms: null,
      finished_at_ms: null,
      status: null,
      segments: [
        { type: "reasoning", part_id: "reasoning-1", text: "inspect" },
        { type: "text", part_id: "text-1", text: "working" },
        { type: "tool_group", tools: [] },
      ],
      usage: null,
      can_fork: false,
      fork_point: null,
      feedback: null,
    } satisfies AssistantMessageSnapshot;

    expect(message.segments.map((segment) => segment.type)).toEqual([
      "reasoning",
      "text",
      "tool_group",
    ]);
  });
});
