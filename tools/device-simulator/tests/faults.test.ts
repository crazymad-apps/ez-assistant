import { describe, expect, it } from "vitest";

import { isImmediateFault, isSimulatorFault } from "../src/node/faults.js";

describe("simulator fault controls", () => {
  it("accepts only the bounded private fault catalogue", () => {
    expect(isSimulatorFault("invalid_next_pcm_sequence")).toBe(true);
    expect(isSimulatorFault("delete_runtime_store")).toBe(false);
  });

  it("distinguishes immediate operations from one-shot wire mutations", () => {
    expect(isImmediateFault("pause_read_5s")).toBe(true);
    expect(isImmediateFault("duplicate_playback_cancel")).toBe(true);
    expect(isImmediateFault("corrupt_next_auth_signature")).toBe(false);
  });
});
