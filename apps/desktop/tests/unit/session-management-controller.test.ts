import { ResourceWorkspaceStore } from "../../src/features/resource-workspace/ResourceWorkspaceStore";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceSummary } from "../../src/generated/assistant-protocol";
import { ConnectionStore } from "../../src/stores/ConnectionStore";
import { NavigationStore } from "../../src/stores/NavigationStore";
import type { RuntimeLifecycleCoordinator } from "../../src/stores/RuntimeLifecycleCoordinator";
import { SessionManagementController } from "../../src/stores/SessionManagementController";

const native = vi.hoisted(() => ({
  chooseWorkspaceDirectory: vi.fn(),
}));

vi.mock("../../src/native-bridge/workspaceDirectory", () => ({
  chooseWorkspaceDirectory: native.chooseWorkspaceDirectory,
}));

describe("SessionManagementController workspace registration", () => {
  beforeEach(() => {
    native.chooseWorkspaceDirectory.mockReset();
    native.chooseWorkspaceDirectory.mockResolvedValue("/workspace/project");
  });

  it("restores a removed workspace without creating an extra session", async () => {
    const fixture = controllerFixture(true);

    await fixture.controller.registerWorkspace({
      label: "project",
      primary_directory: "/workspace/project",
      additional_directories: ["/workspace/docs"],
    });

    expect(fixture.command).toHaveBeenCalledTimes(1);
    expect(fixture.command).toHaveBeenCalledWith({
      type: "register_workspace",
      payload: {
        label: "project",
        primary_directory: "/workspace/project",
        additional_directories: ["/workspace/docs"],
      },
    });
    expect(fixture.load_application).toHaveBeenCalledOnce();
    expect(fixture.select_initial_session).not.toHaveBeenCalled();
    expect(fixture.select_draft).toHaveBeenCalledWith("workspace-1");
    expect(fixture.select_session).not.toHaveBeenCalled();
    expect(fixture.navigation.expanded_workspaces.has("workspace-1")).toBe(true);
  });

  it("opens a draft without creating the initial session for a newly registered workspace", async () => {
    const fixture = controllerFixture(false);

    await fixture.controller.registerWorkspace({
      label: "project",
      primary_directory: "/workspace/project",
      additional_directories: [],
    });

    expect(fixture.command).toHaveBeenCalledTimes(1);
    expect(fixture.select_initial_session).not.toHaveBeenCalled();
    expect(fixture.select_session).not.toHaveBeenCalled();
    expect(fixture.select_draft).toHaveBeenCalledWith("workspace-1");
  });

  it("refreshes the selected Session view after a Workspace edit", async () => {
    const fixture = controllerFixture(false);
    fixture.command.mockReset().mockResolvedValue({
      payload: {
        workspace: {
          workspace_id: "workspace-1",
          label: "renamed",
          user_directory: "/workspace/new-primary",
          additional_directories: ["/workspace/docs"],
        },
      },
    });
    fixture.navigation.selectSession("session-current");

    await fixture.controller.updateWorkspace({
      workspace_id: "workspace-1",
      label: "renamed",
      primary_directory: "/workspace/new-primary",
      additional_directories: ["/workspace/docs"],
    });

    expect(fixture.load_application).toHaveBeenCalledOnce();
    expect(fixture.load_session).toHaveBeenCalledWith("session-current");
  });
});

function controllerFixture(restored: boolean) {
  const workspace: WorkspaceSummary = {
    workspace_id: "workspace-1",
    label: "project",
    user_directory: "/workspace/project",
    additional_directories: [],
    agent_directory: "/runtime/workspaces/workspace-1",
    lifecycle: "active",
    created_at_ms: 1,
    updated_at_ms: 2,
    removed_at_ms: null,
  };
  const command = vi.fn()
    .mockResolvedValueOnce({ payload: { workspace, restored } })
    .mockResolvedValueOnce({ payload: { session: { session_id: "session-1" } } });
  const load_application = vi.fn().mockResolvedValue(undefined);
  const load_session = vi.fn().mockResolvedValue(undefined);
  const select_initial_session = vi.fn().mockResolvedValue(undefined);
  const runtime = {
    client: { command },
    loadApplication: load_application,
    loadSession: load_session,
    selectInitialSession: select_initial_session,
  } as unknown as RuntimeLifecycleCoordinator;
  const navigation = new NavigationStore();
  const connection = new ConnectionStore();
  connection.markConnected("instance-1", {
    protocol_version: 1,
    runtime_version: "test",
    max_command_bytes: 64 * 1024,
    max_attachment_bytes: null,
    sse: true,
    streaming_upload: true,
    features: [],
  });
  const select_session = vi.fn().mockResolvedValue(undefined);
  const select_draft = vi.fn();
  const state = {
    composer_pending: false,
    interaction_error: null,
    pending_session_action: false,
    pending_workspace_action: false,
    pending_proxy_session_id: null,
    pending_compaction_session_id: null,
    pending_compaction_cancel_session_id: null,
    session_notice: null,
  };
  const resources = new ResourceWorkspaceStore();
  const controller = new SessionManagementController({
    resources,
    connection,
    navigation,
    runtime,
    save_preferences: vi.fn(),
    select_draft,
    select_session,
    state,
  });
  return {
    resources, state,
    command,
    controller,
    load_application,
    load_session,
    navigation,
    select_initial_session,
    select_draft,
    select_session,
  };
}

it("closes every terminal owned by a deleted session before consuming its confirmation token", async () => {
  const fixture = controllerFixture(false);
  fixture.resources.selectScope("session:delete-me");
  fixture.resources.openWorkspace("session:delete-me");
  const prepared = { session: { session_id: "delete-me" }, confirmation_token: "confirmed" } as Parameters<SessionManagementController["deleteSession"]>[0];
  let cleanup!: () => void;
  const close = vi.spyOn(fixture.resources, "closeScopeTerminals").mockImplementation(() => new Promise<void>((resolve) => { cleanup = resolve; }));
  fixture.command.mockReset().mockResolvedValue({});
  const deleting = fixture.controller.deleteSession(prepared);
  expect(close).toHaveBeenCalledWith("session:delete-me");
  expect(fixture.command).not.toHaveBeenCalled();
  cleanup();
  expect(await deleting).toBe(true);
  expect(fixture.command).toHaveBeenCalledWith({ type: "delete_session", payload: { session_id: "delete-me", confirmation_token: "confirmed" } });
  expect(fixture.resources.groups.has("session:delete-me")).toBe(false);
});

it("does not consume the delete token or discard indexes when terminal cleanup fails", async () => {
  const fixture = controllerFixture(false);
  fixture.resources.selectScope("session:keep-me");
  fixture.resources.openWorkspace("session:keep-me");
  const prepared = { session: { session_id: "keep-me" }, confirmation_token: "unused" } as Parameters<SessionManagementController["deleteSession"]>[0];
  vi.spyOn(fixture.resources, "closeScopeTerminals").mockRejectedValue(new Error("PTY cleanup failed"));
  expect(await fixture.controller.deleteSession(prepared)).toBe(false);
  expect(fixture.command).not.toHaveBeenCalled();
  expect(fixture.resources.groups.has("session:keep-me")).toBe(true);
  expect(fixture.state.interaction_error).toBe("PTY cleanup failed");
});
