import { afterEach, expect, it, vi } from "vitest";
import { NativeResourceFailure } from "../../src/native-bridge/nativeResource";
import { RootStore } from "../../src/stores/RootStore";
import { RuntimeLifecycleCoordinator } from "../../src/stores/RuntimeLifecycleCoordinator";

afterEach(() => vi.restoreAllMocks());

it("freezes an unknown first send and recovers by the same key without uploading again", async () => {
  const committed = {
    session: { session_id: "committed-session", workspace_id: null },
    input_id: "first-input", run: { run_id: "first-run" }, attachments: [],
  };
  const command = vi.fn(async (request: { type: string }) => request.type === "list_skills"
    ? { payload: { snapshot: { skills: [] } } }
    : { payload: { materialization: committed } });
  vi.spyOn(RuntimeLifecycleCoordinator.prototype, "client", "get")
    .mockReturnValue({ command } as never);
  vi.spyOn(RuntimeLifecycleCoordinator.prototype, "loadApplication").mockResolvedValue();
  vi.spyOn(RuntimeLifecycleCoordinator.prototype, "loadSession").mockResolvedValue();
  const store = new RootStore();
  store.connection.markConnected("instance-1", {
    mode: "personal", min_compatible_version: "0.28.0", runtime_version: "0.28.0",
    max_command_bytes: 65536, max_attachment_bytes: null, sse: true, streaming_upload: true,
    features: ["session_materialization"],
  });
  store.openNewSessionDraft(null);
  store.new_session_drafts.updateText("unbound", "original request");
  const upload = vi.spyOn(store.files, "materializeNewSession")
    .mockRejectedValueOnce(new NativeResourceFailure("response lost", "materialization_response_unknown"));
  expect(await store.materializeNewSessionDraft("unbound")).toBe(false);
  const frozen = store.new_session_drafts.get("unbound")?.materialization_attempt;
  expect(frozen?.message).toBe("original request");
  store.new_session_drafts.updateText("unbound", "must not replace");
  await store.clearNewSessionDraft("unbound");
  expect(store.new_session_drafts.get("unbound")?.text).toBe("original request");
  expect(store.new_session_drafts.get("unbound")?.materialization_attempt).toBe(frozen);

  expect(await store.materializeNewSessionDraft("unbound")).toBe(true);
  expect(command).toHaveBeenCalledWith({ type: "get_session_materialization",
    payload: { idempotency_key: frozen?.idempotency_key } });
  expect(upload).toHaveBeenCalledTimes(1);
  expect(store.navigation.selected_session_id).toBe("committed-session");
  expect(store.new_session_drafts.get("unbound")).toBeNull();
  store.dispose();
});

it("keeps the original manifest when lookup is empty and a later retry cannot stage attachments", async () => {
  const command = vi.fn(async (request: { type: string }) => request.type === "list_skills"
    ? { payload: { snapshot: { skills: [] } } }
    : { payload: { materialization: null } });
  vi.spyOn(RuntimeLifecycleCoordinator.prototype, "client", "get")
    .mockReturnValue({ command } as never);
  const store = new RootStore();
  store.connection.markConnected("instance-1", {
    mode: "personal", min_compatible_version: "0.28.0", runtime_version: "0.28.0",
    max_command_bytes: 65536, max_attachment_bytes: null, sse: true, streaming_upload: true,
    features: ["session_materialization"],
  });
  store.openNewSessionDraft(null);
  store.new_session_drafts.updateText("unbound", "same request");
  const upload = vi.spyOn(store.files, "materializeNewSession")
    .mockRejectedValueOnce(new NativeResourceFailure("response lost", "materialization_response_unknown"))
    .mockRejectedValueOnce(new Error("attachment selection expired"));
  expect(await store.materializeNewSessionDraft("unbound")).toBe(false);
  const original = store.new_session_drafts.get("unbound")?.materialization_attempt;
  expect(await store.materializeNewSessionDraft("unbound")).toBe(false);
  expect(upload).toHaveBeenCalledTimes(2);
  expect(upload.mock.calls[0][0]).toBe(original);
  expect(upload.mock.calls[1][0]).toBe(original);
  expect(store.new_session_drafts.get("unbound")?.materialization_attempt).toBe(original);
  expect(store.interaction_error).toContain("attachment selection expired");
  store.dispose();
});
