import { describe, expect, it } from "vitest";
import type {
  PermissionDocumentSnapshot,
  RuntimeCommand,
} from "../../src/generated/assistant-protocol";
import type { RuntimeClient } from "../../src/runtime-client/RuntimeClient";
import { SettingsStore } from "../../src/stores/SettingsStore";

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
