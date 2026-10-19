import { describe, expect, it, vi } from "vitest";
import { materializeNewSession } from "../../src/native-bridge/nativeResource";
vi.mock("../../src/native-bridge/nativeResource", async (original) => ({
  ...await original<typeof import("../../src/native-bridge/nativeResource")>(),
  materializeNewSession: vi.fn(),
}));
import {
  draftKeyForWorkspace,
  isDraftCustomized,
  NewSessionDraftStore,
} from "../../src/stores/NewSessionDraftStore";
import { RootStore } from "../../src/stores/RootStore";
import { RuntimeLifecycleCoordinator } from "../../src/stores/RuntimeLifecycleCoordinator";

describe("NewSessionDraftStore", () => {
  it("keeps following-default intent and treats explicit choices as customized", () => {
    const drafts = new NewSessionDraftStore();
    const draft = drafts.open("unbound");
    expect(draft.approval_mode).toBe("auto");
    expect(draft.model_selection).toBeNull();
    expect(isDraftCustomized(draft)).toBe(false);
    drafts.updateModel("unbound", { provider_instance_id: "provider-1", model_id: "vendor/model.v1" });
    expect(isDraftCustomized(drafts.get("unbound")!)).toBe(true);
    drafts.updateModel("unbound", null);
    expect(isDraftCustomized(drafts.get("unbound")!)).toBe(false);
  });

  it("copies current device defaults only into drafts created afterwards", () => {
    const drafts = new NewSessionDraftStore();
    const remembered = { provider_instance_id: "provider-1", model_id: "model-a" };
    drafts.applyDefaults("auto", remembered);
    const first = drafts.open("unbound");
    expect(first).toMatchObject({ approval_mode: "auto", model_selection: remembered });
    expect(isDraftCustomized(first, "auto", remembered)).toBe(false);

    drafts.applyDefaults("ask", null);
    const second = drafts.open("workspace:workspace-2");
    expect(second).toMatchObject({ approval_mode: "ask", model_selection: null });
    expect(drafts.get("unbound")).toMatchObject({ approval_mode: "auto", model_selection: remembered });
    expect(isDraftCustomized(first, "ask", null)).toBe(true);
  });

  it("clears an unavailable inherited model only from blank unmaterialized drafts", () => {
    const drafts = new NewSessionDraftStore();
    const remembered = { provider_instance_id: "provider-1", model_id: "model-a" };
    drafts.applyDefaults("ask", remembered);
    drafts.open("unbound");
    drafts.open("workspace:workspace-2");
    drafts.updateText("workspace:workspace-2", "保留用户草稿");

    drafts.discardUnavailableDefaultModel(remembered);

    expect(drafts.default_model_selection).toBeNull();
    expect(drafts.get("unbound")?.model_selection).toBeNull();
    expect(drafts.get("workspace:workspace-2")?.model_selection).toEqual(remembered);
  });
  it("passes a stable MCP key in first materialization and preserves it on rejection", async () => {
    const client = { command: vi.fn(async (request: { type: string }) => request.type === "list_skills"
      ? { payload: { snapshot: { skills: [{ name: "review" }] } } }
      : { payload: { servers: [{ server_key: "github", display_name: "GitHub" }] } }) };
    const clientSpy = vi.spyOn(RuntimeLifecycleCoordinator.prototype, "client", "get")
      .mockReturnValue(client as never);
    const store = new RootStore();
    store.connection.markConnected("instance-1", {
      mode: "personal", min_compatible_version: "0.25.2", runtime_version: "0.25.2", max_command_bytes: 65536,
      max_attachment_bytes: null, sse: true, streaming_upload: true, features: ["session_materialization"],
    });
    store.openNewSessionDraft(null);
    store.new_session_drafts.updateText("unbound", "创建 issue");
    store.new_session_drafts.updateGoalArmed("unbound", true);
    store.new_session_drafts.updateSelectedSkill("unbound", "review");
    store.new_session_drafts.updateSelectedMcp("unbound", { server_key: "github", display_name: "旧展示名" });
    await vi.waitFor(() => expect(store.new_session_drafts.get("unbound")?.skill_status).toBe("ready"));
    vi.mocked(materializeNewSession).mockRejectedValueOnce(new Error("MCP 服务已不可用，请重新选择"));
    expect(await store.materializeNewSessionDraft("unbound")).toBe(false);
    expect(materializeNewSession).toHaveBeenCalledWith(expect.objectContaining({ mcp_server_key: "github", skill_name: "review", mode: "start_goal", message: "创建 issue" }), expect.any(String), expect.objectContaining({ binding: null }));
    expect(store.new_session_drafts.get("unbound")).toMatchObject({ selected_mcp: { server_key: "github" }, goal_armed: true, selected_skill_name: "review" });
    expect(store.interaction_error).toContain("重新选择");
    store.new_session_drafts.updateVariant("unbound", "plan");
    expect(store.new_session_drafts.get("unbound")).toMatchObject({ selected_mcp: null, selected_skill_name: "review", goal_armed: true });
    store.dispose();
    clientSpy.mockRestore();
  });

  it("keeps one isolated in-memory draft per workspace and no Session identity", () => {
    const drafts = new NewSessionDraftStore();
    const first = draftKeyForWorkspace("workspace-1");
    const second = draftKeyForWorkspace("workspace-2");

    drafts.open(first);
    drafts.updateText(first, "保留这段草稿");
    drafts.updateGoalArmed(first, true);
    drafts.open(second);
    drafts.updateText(second, "另一个工作空间");

    expect(drafts.get(first)).toMatchObject({
      workspace_id: "workspace-1",
      text: "保留这段草稿",
      goal_armed: true,
    });
    expect(drafts.get(second)).toMatchObject({
      workspace_id: "workspace-2",
      text: "另一个工作空间",
    });
    expect(drafts.get(first)).not.toHaveProperty("session_id");
  });

  it("keeps a materialization manifest frozen until explicitly cleared", () => {
    const drafts = new NewSessionDraftStore();
    drafts.open("unbound");
    const manifest = {
      idempotency_key: "attempt-1",
      model_selection: { provider_instance_id: "provider-1", model_id: "model-a" },
      variant: "build" as const,
      approval_mode: "ask" as const,
      message: "首次发送",
      mode: "normal" as const,
      attachments: [],
      quotes: [],
    };

    drafts.beginMaterialization("unbound", manifest);
    drafts.setAttachmentTransferState("unbound", "failed", "结果未知");
    expect(drafts.get("unbound")?.materialization_attempt).toEqual(manifest);

    drafts.updateText("unbound", "用户已修改");
    expect(drafts.get("unbound")?.text).toBe("");
    expect(drafts.get("unbound")?.materialization_attempt).toEqual(manifest);

    drafts.clearMaterializationAttempt("unbound");
    expect(drafts.get("unbound")?.materialization_attempt).toBeNull();
    drafts.updateText("unbound", "用户已修改");
    expect(drafts.get("unbound")?.text).toBe("用户已修改");
  });

  it("keeps an agent selection with a moved draft and freezes its source on first send", async () => {
    const drafts = new NewSessionDraftStore();
    drafts.open("unbound");
    const selected = { name: "Warehouse", source: { type: "center" as const, center_id: "enterprise-1" } };
    drafts.updateText("unbound", "画仓库模型");
    drafts.updateSelectedAgent("unbound", selected);
    expect(isDraftCustomized(drafts.get("unbound")!)).toBe(true);
    expect(drafts.move("unbound", "workspace:workspace-1")).toBe(true);
    expect(drafts.get("workspace:workspace-1")?.selected_agent).toEqual(selected);

    const clientSpy = vi.spyOn(RuntimeLifecycleCoordinator.prototype, "client", "get")
      .mockReturnValue({ command: vi.fn() } as never);
    const store = new RootStore();
    store.connection.markConnected("instance-1", {
      mode: "personal", min_compatible_version: "0.28.0", runtime_version: "0.28.0",
      max_command_bytes: 65536, max_attachment_bytes: null, sse: true, streaming_upload: true,
      features: ["session_materialization"],
    });
    store.openNewSessionDraft(null);
    store.new_session_drafts.updateText("unbound", "画仓库模型");
    store.new_session_drafts.updateSelectedAgent("unbound", selected);
    const upload = vi.spyOn(store.files, "materializeNewSession").mockRejectedValueOnce(new Error("test rejected"));
    expect(await store.materializeNewSessionDraft("unbound")).toBe(false);
    expect(upload.mock.calls[0][0].custom_agent).toEqual(selected);
    expect(store.new_session_drafts.get("unbound")?.selected_agent).toEqual(selected);
    store.dispose();
    clientSpy.mockRestore();
  });

  it("does not fall back to creating an empty Session when the Host lacks materialization", async () => {
    const store = new RootStore();
    store.connection.markConnected("instance-1", {
      mode: "personal", min_compatible_version: "0.25.2",
      runtime_version: "old-host",
      max_command_bytes: 64 * 1024,
      max_attachment_bytes: null,
      sse: true,
      streaming_upload: true,
      features: [],
    });
    store.openNewSessionDraft(null);
    store.new_session_drafts.updateText("unbound", "不会创建空会话");

    expect(await store.materializeNewSessionDraft("unbound")).toBe(false);
    expect(store.interaction_error).toContain("重启或完成应用更新");
    expect(store.navigation.selected_session_id).toBeNull();
    expect(store.new_session_drafts.get("unbound")?.text).toBe("不会创建空会话");
  });
});

vi.mock("../../src/runtime-client/ClientResources", async (original) => {
  const actual = await original<typeof import("../../src/runtime-client/ClientResources")>();
  return {...actual, ClientResources: class extends actual.ClientResources { override readonly desktop = true; }};
});
