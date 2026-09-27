import { describe, expect, it } from "vitest";
import { NewSessionDraftStore } from "../../src/stores/NewSessionDraftStore";
import { ResourceWorkspaceStore } from "../../src/features/resource-workspace/ResourceWorkspaceStore";

describe("new-session draft movement", () => {
  it("moves one frozen owner snapshot, keeps selections, and invalidates workspace skill options", () => {
    const drafts = new NewSessionDraftStore();
    drafts.open("unbound");
    drafts.updateText("unbound", "keep this text");
    drafts.updateAttachments("unbound", [{ selection_id: "selection-1", original_name: "a.txt", size_bytes: 1,
      media_type: null, origin: "file_picker", state: "selected", attachment_id: null, error: null, operation_id: null }]);
    drafts.updateSelectedSkill("unbound", "existing-skill");
    drafts.open("workspace:target");
    expect(drafts.move("unbound", "workspace:target")).toBe(true);
    expect(drafts.get("unbound")).toBeNull();
    expect(drafts.get("workspace:target")).toMatchObject({
      key: "workspace:target", workspace_id: "target", text: "keep this text",
      selected_skill_name: "existing-skill", skill_options: [], skill_status: "idle",
      attachments: [{ selection_id: "selection-1" }],
    });
    drafts.applySkillOptions("workspace:target", []);
    expect(drafts.get("workspace:target")?.selected_skill_name).toBe("existing-skill");
  });

  it("rejects destination content and never thaws an unresolved materialization", () => {
    const drafts = new NewSessionDraftStore();
    drafts.open("unbound");
    drafts.updateText("unbound", "first");
    drafts.open("workspace:target");
    drafts.updateText("workspace:target", "second");
    expect(drafts.move("unbound", "workspace:target")).toBe(false);
    drafts.updateText("workspace:target", "");
    drafts.beginMaterialization("unbound", {
      idempotency_key: "first-key", message: "first", variant: "build", approval_mode: "ask", mode: "normal",
    });
    drafts.updateText("unbound", "changed");
    expect(drafts.get("unbound")?.text).toBe("first");
    expect(drafts.get("unbound")?.materialization_attempt?.idempotency_key).toBe("first-key");
    expect(drafts.move("unbound", "workspace:target")).toBe(false);
  });

  it("retains the resource group and page identity and rejects occupied target resources", () => {
    const resources = new ResourceWorkspaceStore();
    resources.selectScope("draft:unbound");
    const source = resources.active_group;
    resources.openWorkspace("draft:unbound");
    const originalTerminalSource = { type: "workspace" as const, workspace_id: "original-workspace" };
    resources.openTerminal(originalTerminalSource, source, true);
    const terminalTab = source.tabs.find((tab) => tab.type === "terminal");
    expect(terminalTab?.type).toBe("terminal");
    const terminalId = terminalTab?.type === "terminal" ? terminalTab.terminalId : "";
    const pageKey = resources.pageKey(source, source.tabs[1]);
    const viewState = resources.pageState(pageKey);
    resources.selectScope("draft:workspace:target");
    expect(resources.moveDraft("unbound", "workspace:target")).toBe(true);
    expect(resources.groups.get("draft:workspace:target")).toBe(source);
    expect(resources.pageKey(source, source.tabs[1])).toBe(pageKey);
    expect(resources.pageState(pageKey)).toBe(viewState);
    expect(resources.terminals.get(terminalId)?.source).toEqual(originalTerminalSource);
    resources.selectScope("draft:workspace:occupied");
    resources.openWorkspace("draft:workspace:occupied");
    expect(resources.moveDraft("workspace:target", "workspace:occupied")).toBe(false);
  });
});
