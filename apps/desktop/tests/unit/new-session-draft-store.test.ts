import { describe, expect, it } from "vitest";
import {
  draftKeyForWorkspace,
  NewSessionDraftStore,
} from "../../src/stores/NewSessionDraftStore";
import { RootStore } from "../../src/stores/RootStore";

describe("NewSessionDraftStore", () => {
  it("keeps one isolated in-memory draft per workspace and no Session identity", () => {
    const drafts = new NewSessionDraftStore();
    const first = draftKeyForWorkspace("workspace-1");
    const second = draftKeyForWorkspace("workspace-2");

    drafts.open(first, "model-a");
    drafts.updateText(first, "保留这段草稿");
    drafts.updateGoalArmed(first, true);
    drafts.open(second, "model-a");
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

  it("retains one materialization manifest until the draft changes or the attempt is cleared", () => {
    const drafts = new NewSessionDraftStore();
    drafts.open("unbound", "model-a");
    const manifest = {
      idempotency_key: "attempt-1",
      model_key: "model-a",
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
    expect(drafts.get("unbound")?.materialization_attempt).toBeNull();

    drafts.beginMaterialization("unbound", manifest);
    drafts.clearMaterializationAttempt("unbound");
    expect(drafts.get("unbound")?.materialization_attempt).toBeNull();
  });

  it("does not fall back to creating an empty Session when the Host lacks materialization", async () => {
    const store = new RootStore();
    store.connection.markConnected("instance-1", {
      protocol_version: 1,
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
