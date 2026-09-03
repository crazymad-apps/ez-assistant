import { describe, expect, it } from "vitest";
import type {
  PermissionDocumentSnapshot,
  RuntimeCommand,
} from "../../src/generated/assistant-protocol";
import type { RuntimeClient } from "../../src/runtime-client/RuntimeClient";
import { SettingsStore } from "../../src/stores/SettingsStore";
import { serverDraft } from "../../src/features/settings/SettingsDialog/McpSettingsPage/draft";

describe("SettingsStore MCP management", () => {
  it("cancels a pending test by its own ID and ignores late results", async () => {
    const commands: RuntimeCommand[] = [];
    let resolve_test: (value: unknown) => void = () => undefined;
    const client = { command: async (command: RuntimeCommand) => {
      commands.push(command);
      if (command.type === "test_mcp_server") return new Promise((resolve) => { resolve_test = resolve; });
      if (command.type === "cancel_mcp_server_test") return { type: command.type, payload: {} };
      throw new Error("unexpected command");
    } } as unknown as RuntimeClient;
    const store = permissionStore(client).mcp;
    const testing = store.test(serverDraft(null));
    expect(store.testing).toBe(true);
    store.cancelTest();
    expect(commands[1]).toMatchObject({ type: "cancel_mcp_server_test", payload: { test_id: commands[0]!.type === "test_mcp_server" ? commands[0]!.payload.test_id : "missing" } });
    resolve_test({ type: "test_mcp_server", payload: { outcome: "success", stage: "complete", elapsed_ms: 5, tool_count: 1 } });
    await testing;
    expect(store.testing).toBe(false);
    expect(store.test_result).toBeNull();
  });

  it("keeps stale configuration on read failure and never displays raw errors", async () => {
    const client = { command: async () => { throw Object.assign(new Error("sensitive-url-token"), { code: "mcp_config_conflict" }); } } as unknown as RuntimeClient;
    const store = permissionStore(client).mcp;
    store.configuration = { revision: "old", needs_refresh: false, servers: [], diagnostics: [] };
    expect(await store.mutate("old", { type: "remove", payload: { server_key: "example" } })).toBe(false);
    expect(store.configuration_conflict).toBe(true);
    expect(store.error_message).not.toContain("sensitive-url-token");
    await store.load();
    expect(store.stale).toBe(true);
    expect(store.configuration?.revision).toBe("old");
    expect(store.error_message).not.toContain("sensitive-url-token");
  });
});

describe("SettingsStore connection validation", () => {
  it("presents a failed validation as a persistent error", async () => {
    const client = {
      command: async () => ({
        type: "validate_model_connection",
        payload: {
          model_key: "qwen3.8-max",
          outcome: {
            status: "failed",
            failure: {
              kind: "authentication",
              message: "provider rejected credential",
            },
          },
        },
      }),
    } as unknown as RuntimeClient;
    const store = new SettingsStore({
      get_client: () => client,
      get_permission_context: () => ({ session_id: null, workspace_id: null }),
      refresh_application: async () => undefined,
    });

    const result = await store.validateConfigured("qwen3.8-max");

    expect(result?.outcome.status).toBe("failed");
    expect(store.error_message).toBe("API Key 无效或无权访问该模型，请检查凭据。");
    expect(store.notice_message).toBeNull();
  });
});

describe("SettingsStore permission management", () => {
  it("loads the session, workspace, and global permission documents for the active context", async () => {
    const commands: RuntimeCommand[] = [];
    const client = {
      command: async (command: RuntimeCommand) => {
        commands.push(command);
        if (command.type !== "get_permission_document") throw new Error("unexpected command");
        return {
          type: command.type,
          payload: { document: permissionDocument(command.payload.scope) },
        };
      },
    } as unknown as RuntimeClient;
    const store = permissionStore(client);

    await store.loadPermissions();

    expect(commands.map((command) => command.type)).toEqual([
      "get_permission_document",
      "get_permission_document",
      "get_permission_document",
    ]);
    expect(store.permission_documents.map((document) => document.scope.type)).toEqual([
      "session",
      "workspace",
      "global",
    ]);
  });

  it("sends the observed revision and keeps a CAS conflict visible for recovery", async () => {
    const conflict = Object.assign(new Error("权限文件已被外部修改"), {
      code: "permission_file_conflict",
    });
    const client = {
      command: async (command: RuntimeCommand) => {
        expect(command).toMatchObject({
          type: "replace_permission_document",
          payload: { expected_revision: { type: "content", payload: { value: "revision-1" } } },
        });
        throw conflict;
      },
    } as unknown as RuntimeClient;
    const store = permissionStore(client);
    const document = permissionDocument({
      type: "session",
      payload: { session_id: "session-1" },
    });
    store.permission_documents = [document];

    const saved = await store.replacePermissionDocument(
      document.scope,
      document.revision,
      { schema_version: 1, rules: [] },
    );

    expect(saved).toBe(false);
    expect(store.permission_conflict).toBe(true);
    expect(store.error_message).toBe("权限文件已被外部修改");
    expect(store.permission_documents).toEqual([document]);
  });
});

describe("SettingsStore skill management", () => {
  it("loads the current body only when a named detail is opened", async () => {
    const commands: RuntimeCommand[] = [];
    const skill = {
      name: "review",
      description: "检查",
      source: "workspace_ez_assistant",
      model_invocable: true,
      user_invocable: true,
      enabled: true,
      health: "ready",
    } as const;
    const client = {
      command: async (command: RuntimeCommand) => {
        commands.push(command);
        if (command.type === "get_skill_detail") {
          return {
            type: command.type,
            payload: { detail: { skill, body: "# 检查步骤", diagnostics: [] } },
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as RuntimeClient;
    const store = permissionStore(client);
    store.skill_workspace_id = "workspace-1";

    await store.loadSkillDetail("review");

    expect(commands[0]).toMatchObject({
      type: "get_skill_detail",
      payload: { workspace_id: "workspace-1", name: "review" },
    });
    expect(store.skill_detail?.body).toBe("# 检查步骤");
  });

  it("reads the selected scope and replaces the projection returned by a name toggle", async () => {
    const commands: RuntimeCommand[] = [];
    const ready = {
      available: true,
      skills: [{ name: "review", description: "检查", source: "workspace_ez_assistant", model_invocable: true, user_invocable: true, enabled: true, health: "ready" }],
      diagnostics: [],
    } as const;
    const disabled = {
      ...ready,
      skills: [{ ...ready.skills[0], enabled: false, health: "disabled" }],
    } as const;
    const client = {
      command: async (command: RuntimeCommand) => {
        commands.push(command);
        if (command.type === "list_skills") return { type: command.type, payload: { snapshot: ready } };
        if (command.type === "set_skill_enabled") return { type: command.type, payload: { snapshot: disabled } };
        throw new Error("unexpected command");
      },
    } as unknown as RuntimeClient;
    const store = permissionStore(client);
    store.skill_workspace_id = "workspace-1";

    await store.loadSkills();
    expect(commands[0]).toMatchObject({ type: "list_skills", payload: { workspace_id: "workspace-1" } });
    expect(store.skill_management?.skills[0]?.enabled).toBe(true);

    await store.setSkillEnabled("review", false);
    expect(commands[1]).toMatchObject({ type: "set_skill_enabled", payload: { workspace_id: "workspace-1", name: "review", enabled: false } });
    expect(store.skill_management?.skills[0]?.enabled).toBe(false);
  });
});

function permissionStore(client: RuntimeClient): SettingsStore {
  return new SettingsStore({
    get_client: () => client,
    get_permission_context: () => ({
      session_id: "session-1",
      workspace_id: "workspace-1",
    }),
    refresh_application: async () => undefined,
  });
}

function permissionDocument(
  scope: PermissionDocumentSnapshot["scope"],
): PermissionDocumentSnapshot {
  return {
    scope,
    revision: { type: "content", payload: { value: "revision-1" } },
    schema_version: 1,
    status: "ready",
    editable: scope.type !== "global",
    rules: [],
    diagnostics: [],
  };
}
