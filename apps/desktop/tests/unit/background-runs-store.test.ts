import { expect, it, vi } from "vitest";
import type { StopBackgroundRunResult } from "@ez-assistant/protocol";
import { BackgroundRunsStore } from "../../src/features/context-panel/BackgroundRunsSection/store";
import type { RuntimeBootstrap } from "../../src/native-bridge/runtimeBootstrap";
import { RuntimeClient } from "../../src/runtime-client/RuntimeClient";

function setupPendingStop() {
  const client = new RuntimeClient({ base_url: "http://fixture", access_token: "fixture", instance_id: "host" } as RuntimeBootstrap);
  vi.spyOn(client, "listBackgroundRuns").mockResolvedValue({ runs: [], offset: 0, total: 0, limit: 5 });
  let resolve_stop!: (result: StopBackgroundRunResult) => void;
  vi.spyOn(client, "stopBackgroundRun").mockReturnValueOnce(new Promise((resolve) => { resolve_stop = resolve; }));
  const runs = new BackgroundRunsStore(client, "session");
  const result = { background_id: "old", exit_code: 7, timed_out: false, output_error: null };
  runs.attach();
  runs.setAvailable(true);
  const pending = runs.stop(result.background_id);
  return { runs, pending, result, resolve_stop };
}

it("clears an old notice on new activity and ignores a late stop response for it", async () => {
  const { runs, pending, result, resolve_stop } = setupPendingStop();
  runs.ended(result.background_id, result);
  expect(runs.notice?.message).toContain("退出码 7");

  runs.changed("new");
  expect(runs.notice).toBeNull();
  resolve_stop(result);
  await pending;

  expect(runs.notice).toBeNull();
  runs.dispose();
});

it("lets the stop response complete a notice that is still being shown", async () => {
  const { runs, pending, result, resolve_stop } = setupPendingStop();
  runs.ended(result.background_id);
  expect(runs.notice?.message).toContain("输出已不可用");

  resolve_stop(result);
  await pending;

  expect(runs.notice?.message).toContain("退出码 7");
  runs.dispose();
});
