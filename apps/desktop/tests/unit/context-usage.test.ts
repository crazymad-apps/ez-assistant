import { describe, expect, it } from "vitest";
import { effectiveContextUsage } from "../../src/stores/contextUsage";

describe("effectiveContextUsage", () => {
  const context = {
    used_tokens: 40,
    window_tokens: 100,
    usage_basis_points: 4_000,
  };
  const live_usage = {
    input_tokens: 65,
    output_tokens: 5,
    total_tokens: 70,
    cached_input_tokens: null,
  };

  it("uses the latest completed step total for the matching active run", () => {
    expect(effectiveContextUsage(context, "run-1", "run-1", live_usage)).toEqual({
      used_tokens: 70,
      window_tokens: 100,
      usage_basis_points: 7_000,
    });
  });

  it("does not let a retained or unrelated live run override reliable context", () => {
    expect(effectiveContextUsage(context, null, "run-1", live_usage)).toBe(context);
    expect(effectiveContextUsage(context, "run-2", "run-1", live_usage)).toBe(context);
  });

  it("does not regress a snapshot that already includes unreported tool-result text", () => {
    expect(effectiveContextUsage(
      { ...context, used_tokens: 75, usage_basis_points: 7_500 },
      "run-1",
      "run-1",
      live_usage,
    )).toEqual({
      used_tokens: 75,
      window_tokens: 100,
      usage_basis_points: 7_500,
    });
  });
});
