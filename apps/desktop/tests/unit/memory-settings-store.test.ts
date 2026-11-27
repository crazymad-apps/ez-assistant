import { describe, expect, it, vi } from "vitest";
import type {
  ControllerPromptSnapshot,
  MemoryCapabilities,
  PinnedMemoryCollectionSnapshot,
  PersonaSnapshot,
  RuntimeCommand,
} from "@ez-assistant/protocol";
import type { RuntimeClient } from "../../src/runtime-client/RuntimeClient";
import { MemorySettingsStore } from "../../src/stores/MemorySettingsStore";

describe("MemorySettingsStore", () => {
  it("loads Persona, Controller prompt, and Pinned Memory as separate resources", async () => {
    const commands: RuntimeCommand[] = [];
    const client = clientWithCommand(async (command) => {
      commands.push(command);
      if (command.type === "get_persona") {
        return {
          type: command.type,
          payload: { persona: persona(), capabilities },
        };
      }
      if (command.type === "get_controller_prompt") {
        return {
          type: command.type,
          payload: { prompt: controllerPrompt(), max_bytes: 16_384 },
        };
      }
      if (command.type === "list_pinned_memories") {
        return {
          type: command.type,
          payload: { collection: collection() },
        };
      }
      throw new Error("unexpected command");
    });
    const store = memoryStore(client);

    await store.load();

    expect(commands.map((command) => command.type)).toEqual([
      "get_persona",
      "get_controller_prompt",
      "list_pinned_memories",
    ]);
    expect(store.persona?.content).toBe("结论优先");
    expect(store.controller_prompt?.content).toBe("简短口语回复");
    expect(store.controller_prompt_max_bytes).toBe(16_384);
    expect(store.collection?.items[0]?.content).toBe("测试后再提交");
    expect(store.capabilities).toEqual(capabilities);
  });

  it("saves Controller prompt with revision CAS and surfaces conflicts", async () => {
    const command = vi
      .fn()
      .mockResolvedValueOnce({
        type: "set_controller_prompt",
        payload: {
          applied: true,
          prompt: controllerPrompt({ revision: 3, enabled: true, content: "一句讲完" }),
        },
      })
      .mockResolvedValueOnce({
        type: "set_controller_prompt",
        payload: {
          applied: false,
          prompt: controllerPrompt({ revision: 4, enabled: false, content: "远端提示词" }),
        },
      });
    const store = memoryStore(clientWithCommand(command));
    store.controller_prompt = controllerPrompt();

    const saved = await store.saveControllerPrompt(true, "一句讲完");
    expect(saved).toBe(true);
    expect(store.controller_prompt?.revision).toBe(3);
    expect(store.notice_message).toContain("下一轮对话立即生效");

    const conflicted = await store.saveControllerPrompt(true, "冲突草稿");
    expect(conflicted).toBe(false);
    expect(store.controller_prompt?.content).toBe("远端提示词");
    expect(store.conflict_message).toContain("草稿仍保留");
  });

  it("uses Persona revision CAS and keeps conflicts visible", async () => {
    const command = vi.fn(async (request: RuntimeCommand) => {
      expect(request).toMatchObject({
        type: "set_persona",
        payload: { expected_revision: 3, enabled: true, content: "新的偏好" },
      });
      return {
        type: "set_persona",
        payload: { applied: false, persona: persona({ revision: 4, content: "远端偏好" }) },
      };
    });
    const store = memoryStore(clientWithCommand(command));
    store.persona = persona();

    const saved = await store.savePersona(true, "新的偏好");

    expect(saved).toBe(false);
    expect(store.persona?.content).toBe("远端偏好");
    expect(store.conflict_message).toContain("草稿仍保留");
  });

  it("uses the collection revision when creating a Pinned Memory", async () => {
    const command = vi.fn(async (request: RuntimeCommand) => {
      expect(request).toMatchObject({
        type: "create_pinned_memory",
        payload: {
          expected_collection_revision: 7,
          category: "协作约定",
          content: "先验证再汇报",
          attributes: {},
        },
      });
      return {
        type: "create_pinned_memory",
        payload: { applied: true, memory: null, collection: collection({ revision: 8 }) },
      };
    });
    const store = memoryStore(clientWithCommand(command));
    store.collection = collection();

    const saved = await store.createPinnedMemory("协作约定", "先验证再汇报");

    expect(saved).toBe(true);
    expect(store.collection?.revision).toBe(8);
    expect(store.notice_message).toBe("Pinned Memory 已添加。");
  });

  it("uses the entry revision when updating and deleting a Pinned Memory", async () => {
    const requests: RuntimeCommand[] = [];
    const command = vi.fn(async (request: RuntimeCommand) => {
      requests.push(request);
      if (request.type === "update_pinned_memory") {
        return {
          type: request.type,
          payload: { applied: true, memory: null, collection: collection({ revision: 8 }) },
        };
      }
      if (request.type === "delete_pinned_memory") {
        return {
          type: request.type,
          payload: { applied: true, collection: collection({ revision: 9, items: [] }) },
        };
      }
      throw new Error("unexpected command");
    });
    const store = memoryStore(clientWithCommand(command));
    store.collection = collection();
    const memory = store.collection.items[0];

    expect(await store.updatePinnedMemory(memory, "协作约定", "先验证再汇报")).toBe(true);
    expect(await store.deletePinnedMemory(memory)).toBe(true);

    expect(requests).toEqual([
      {
        type: "update_pinned_memory",
        payload: {
          id: "memory-1",
          expected_revision: 1,
          category: "协作约定",
          content: "先验证再汇报",
          attributes: {},
        },
      },
      {
        type: "delete_pinned_memory",
        payload: { id: "memory-1", expected_revision: 1 },
      },
    ]);
    expect(store.collection?.items).toEqual([]);
  });
});

const capabilities: MemoryCapabilities = {
  max_persona_bytes: 16_384,
  max_pinned_entries: 200,
  max_pinned_category_bytes: 128,
  max_pinned_content_bytes: 16_384,
  max_attributes_per_entry: 16,
  max_attribute_key_bytes: 64,
  max_attribute_string_bytes: 512,
};

function persona(overrides: Partial<PersonaSnapshot> = {}): PersonaSnapshot {
  return {
    enabled: true,
    content: "结论优先",
    revision: 3,
    updated_at_ms: 1,
    ...overrides,
  };
}

function controllerPrompt(
  overrides: Partial<ControllerPromptSnapshot> = {},
): ControllerPromptSnapshot {
  return {
    enabled: true,
    content: "简短口语回复",
    revision: 2,
    updated_at_ms: 1,
    ...overrides,
  };
}

function collection(
  overrides: Partial<PinnedMemoryCollectionSnapshot> = {},
): PinnedMemoryCollectionSnapshot {
  return {
    revision: 7,
    capabilities,
    items: [{
      id: "memory-1",
      category: "协作约定",
      content: "测试后再提交",
      attributes: {},
      created_by: { type: "user" },
      created_at_ms: 1,
      updated_at_ms: 1,
      revision: 1,
    }],
    ...overrides,
  };
}

function memoryStore(client: RuntimeClient): MemorySettingsStore {
  return new MemorySettingsStore({ get_client: () => client });
}

function clientWithCommand(
  command: (request: RuntimeCommand) => Promise<unknown>,
): RuntimeClient {
  return { command } as unknown as RuntimeClient;
}
