import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SettingsDialog } from "../../src/features/settings/SettingsDialog";
import type {
  ApplicationSnapshot,
  MemoryCapabilities,
  ModelCatalogSnapshot,
  ModelConfiguration,
  PersonaSnapshot,
  PinnedMemoryCollectionSnapshot,
  PermissionDocumentSnapshot,
  SessionSummary,
} from "../../src/generated/assistant-protocol";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("SettingsDialog model management", () => {
  it("shows the Chinese skill management projection and delegates the name toggle", () => {
    const store = settingsStore();
    store.settings.page = "skills";
    store.settings.skill_management = {
      available: true,
      skills: [{ name: "review", description: "检查实现", source: "workspace_ez_assistant", model_invocable: true, user_invocable: true, enabled: true, health: "ready" }],
      diagnostics: [],
    };
    const toggle = vi.spyOn(store.settings, "setSkillEnabled").mockResolvedValue(true);
    renderDialog(store);

    expect(screen.getByRole("button", { name: /运行时/ })).toBeVisible();
    expect(screen.queryByText("文件与启停变更仅对新会话生效。")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "技能工作区范围" })).toHaveTextContent("仅用户根目录");
    expect(screen.getByText("检查实现")).toBeVisible();
    fireEvent.click(screen.getByRole("checkbox"));
    expect(toggle).toHaveBeenCalledWith("review", false);
  });

  it("moves the summary into the description row and renders the skill body", () => {
    const store = settingsStore();
    const skill = { name: "review", description: "检查实现", source: "workspace_ez_assistant", model_invocable: true, user_invocable: true, enabled: true, health: "ready" } as const;
    store.settings.page = "skills";
    store.settings.skill_management = { available: true, skills: [skill], diagnostics: [] };
    store.settings.skill_detail = { skill, body: "# 检查步骤\n\n确认实现与测试。", diagnostics: [] };
    const load_detail = vi.spyOn(store.settings, "loadSkillDetail").mockResolvedValue();
    renderDialog(store);

    fireEvent.click(screen.getByRole("button", { name: /review/ }));

    expect(load_detail).toHaveBeenCalledWith("review");
    expect(screen.getByRole("heading", { name: "review" })).toBeVisible();
    expect(screen.getByText("描述").nextElementSibling).toHaveTextContent("检查实现");
    expect(screen.getByRole("heading", { name: "技能正文" })).toBeVisible();
    expect(screen.getByRole("heading", { name: "检查步骤" })).toBeVisible();
    expect(screen.getByText("确认实现与测试。")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "返回技能列表" }));
    expect(screen.getByRole("heading", { name: "技能" })).toBeVisible();
  });

  it("keeps management actions available for existing and default models", () => {
    const store = settingsStore();
    const existing_default = model("primary", true);
    existing_default.editable = false;
    existing_default.deletable = false;
    store.settings.models = [existing_default];
    renderDialog(store);

    fireEvent.click(screen.getByRole("button", { name: "primary的更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "编辑模型" }));

    expect(screen.getByRole("heading", { name: "编辑模型" })).toBeInTheDocument();
    expect(screen.getByLabelText("显示名称")).toHaveValue("primary");
    fireEvent.click(screen.getByRole("button", { name: "返回模型列表" }));
    expect(screen.getByRole("heading", { name: "模型" })).toBeVisible();
  });

  it("selects the default auxiliary vision model from compiled image-capable models", async () => {
    const store = settingsStore();
    const text_model = model("text-only", true);
    const vision_model = model("vision", false);
    vision_model.supports_image_input = true;
    store.settings.models = [text_model, vision_model];
    const set_vision = vi.spyOn(store.settings, "setAuxiliaryVisionModel").mockResolvedValue(true);
    renderDialog(store);

    const trigger = screen.getByRole("button", { name: "默认识图模型" });
    expect(trigger).toHaveTextContent("未配置");
    fireEvent.click(trigger);
    const listbox = screen.getByRole("listbox", { name: "默认识图模型" });
    expect(within(listbox).queryByRole("option", { name: /text-only/ })).not.toBeInTheDocument();
    fireEvent.click(within(listbox).getByRole("option", { name: /vision/ }));

    await waitFor(() => expect(set_vision).toHaveBeenCalledWith("vision"));
  });

  it("submits a new model through the shared Runtime candidate shape", async () => {
    const store = settingsStore();
    const create = vi.spyOn(store.settings, "createModel").mockResolvedValue(true);
    renderDialog(store);

    fireEvent.click(screen.getByRole("button", { name: "添加模型" }));
    const protocol = screen.getByRole("button", { name: "选择接口协议" });
    fireEvent.click(protocol);
    expect(within(screen.getByRole("listbox", { name: "选择接口协议" })).getAllByRole("option").map((option) => option.textContent)).toEqual([
      "Chat Completions（OpenAI Compatible）",
    ]);
    fireEvent.click(protocol);
    const provider = screen.getByRole("combobox", { name: "供应商（Provider）" });
    fireEvent.click(provider);
    expect(within(screen.getByRole("listbox", { name: "供应商（Provider）" })).getAllByRole("option").map((option) => option.textContent)).toEqual([
      "DeepSeekdeepseek",
      "智谱 GLMzhipu",
      "阿里云百炼（Qwen）dashscope",
      "OpenAIopenai",
      "Moonshot（Kimi）moonshot",
    ]);
    fireEvent.click(within(screen.getByRole("listbox", { name: "供应商（Provider）" })).getByRole("option", { name: /DeepSeek/ }));
    expect(provider).toHaveValue("deepseek");
    expect(screen.getByLabelText("模型 Key")).toHaveAttribute(
      "placeholder",
      "应用内唯一标识，例如：deepseek-v4-pro",
    );
    expect(screen.getByRole("combobox", { name: "模型 ID" })).toHaveAttribute(
      "placeholder",
      "供应商接口使用的模型名，例如：deepseek-v4-pro",
    );
    fireEvent.change(screen.getByLabelText("显示名称"), { target: { value: "Fixture Pro" } });
    fireEvent.change(screen.getByLabelText("模型 Key"), { target: { value: "fixture-pro" } });
    fireEvent.change(screen.getByRole("combobox", { name: "模型 ID" }), { target: { value: "fixture-pro-model" } });
    fireEvent.change(screen.getByLabelText("Endpoint"), { target: { value: "http://api.example.test/v1" } });
    fireEvent.change(screen.getByLabelText("API Key"), { target: { value: "form-secret" } });
    fireEvent.click(screen.getByLabelText("设为默认模型"));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(create).toHaveBeenCalledWith(expect.objectContaining({
      model_key: "fixture-pro",
      protocol: "openai_chat_completions",
      provider: "deepseek",
      endpoint: "http://api.example.test/v1",
      credential: { mode: "replace", value: "form-secret" },
    }), true));
    expect(screen.queryByLabelText("API Key")).not.toBeInTheDocument();
  });

  it("accepts a dotted model key used by model vendors", async () => {
    const store = settingsStore();
    const create = vi.spyOn(store.settings, "createModel").mockResolvedValue(true);
    renderDialog(store);

    fireEvent.click(screen.getByRole("button", { name: "添加模型" }));
    fireEvent.change(screen.getByLabelText("显示名称"), { target: { value: "Qwen Max" } });
    fireEvent.change(screen.getByLabelText("模型 Key"), { target: { value: "qwen3.8-max" } });
    fireEvent.change(screen.getByRole("combobox", { name: "模型 ID" }), { target: { value: "qwen3.8-max" } });
    fireEvent.change(screen.getByLabelText("Endpoint"), { target: { value: "https://api.example.test/v1" } });
    fireEvent.change(screen.getByLabelText("API Key"), { target: { value: "form-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(create).toHaveBeenCalledWith(expect.objectContaining({
      model_key: "qwen3.8-max",
      model: "qwen3.8-max",
    }), false));
  });

  it("supplies a valid replacement when deleting the default model", async () => {
    const store = settingsStore();
    store.settings.models = [model("primary", true), model("secondary", false)];
    const remove = vi.spyOn(store.settings, "deleteModel").mockResolvedValue(true);
    renderDialog(store);

    fireEvent.click(screen.getByRole("button", { name: "primary的更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "删除模型" }));
    const dialog = screen.getByRole("dialog", { name: "删除模型" });
    fireEvent.click(within(dialog).getByRole("button", { name: "选择替代默认模型" }));
    fireEvent.click(within(screen.getByRole("listbox", { name: "选择替代默认模型" })).getByRole("option", { name: "secondary" }));
    fireEvent.click(within(dialog).getByRole("button", { name: "删除模型" }));
    await waitFor(() => expect(remove).toHaveBeenCalledWith("primary", "secondary"));
  });

  it("allows deletion for an idle referencing session but blocks a running one", async () => {
    const idle_store = settingsStore();
    idle_store.settings.models = [model("primary", true), model("secondary", false)];
    idle_store.projection.application = applicationWithSession(modelSession(null));
    const remove = vi.spyOn(idle_store.settings, "deleteModel").mockResolvedValue(true);
    const idle_view = renderDialog(idle_store);

    fireEvent.click(screen.getByRole("button", { name: "secondary的更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "删除模型" }));
    const idle_dialog = screen.getByRole("dialog", { name: "删除模型" });
    expect(idle_dialog).toHaveTextContent("空闲会话将变为未选择模型");
    fireEvent.click(within(idle_dialog).getByRole("button", { name: "删除模型" }));
    await waitFor(() => expect(remove).toHaveBeenCalledWith("secondary", null));
    idle_view.unmount();

    const running_store = settingsStore();
    running_store.settings.models = [model("primary", true), model("secondary", false)];
    running_store.projection.application = applicationWithSession(modelSession("run-1"));
    renderDialog(running_store);
    fireEvent.click(screen.getByRole("button", { name: "secondary的更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "删除模型" }));
    const running_dialog = screen.getByRole("dialog", { name: "删除模型" });
    expect(running_dialog).toHaveTextContent("正在执行或存在排队输入");
    expect(within(running_dialog).queryByRole("button", { name: "删除模型" })).not.toBeInTheDocument();
  });
});

describe("SettingsDialog permission management", () => {
  it("uses shared selection popovers and keeps scope tabs concise", () => {
    const store = settingsStore();
    store.settings.page = "permissions";
    store.settings.permission_documents = [
      permissionDocument({ type: "session", payload: { session_id: "session-1" } }, true),
      permissionDocument({ type: "workspace", payload: { workspace_id: "workspace-1" } }, true),
      permissionDocument({ type: "global" }, false),
    ];
    renderDialog(store);

    const scope_tabs = screen.getByRole("tablist");
    expect(scope_tabs.closest("header")).not.toBeNull();
    expect(within(scope_tabs).getByRole("tab", { name: "当前会话" })).toHaveTextContent(/^当前会话$/);
    expect(within(scope_tabs).getByRole("tab", { name: "当前会话" })).toHaveAttribute("aria-selected", "true");
    expect(within(scope_tabs).getByRole("tab", { name: "工作区" })).toHaveTextContent(/^工作区$/);
    expect(within(scope_tabs).getByRole("tab", { name: "全局" })).toHaveTextContent(/^全局$/);

    fireEvent.click(screen.getByRole("button", { name: "添加规则" }));
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();

    const effect = screen.getByRole("button", { name: "选择规则效果" });
    fireEvent.click(effect);
    const listbox = screen.getByRole("listbox");
    expect(within(listbox).getAllByRole("option").map((option) => option.textContent)).toEqual([
      "允许",
      "询问",
      "拒绝",
    ]);
    fireEvent.click(within(listbox).getByRole("option", { name: "拒绝" }));
    expect(effect).toHaveTextContent("拒绝");
  });
});

describe("SettingsDialog memory management", () => {
  it("shows a concise empty state and opens the shared inline editor", () => {
    const store = settingsStore();
    store.settings.page = "memory";
    store.memory_settings.persona = personaSnapshot();
    store.memory_settings.capabilities = memoryCapabilities;
    store.memory_settings.collection = pinnedCollection();
    renderDialog(store);

    expect(screen.getByText("尚未添加 Pinned Memory。")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "添加" }));
    expect(screen.getByText("添加 Pinned Memory")).toBeInTheDocument();
    expect(screen.getByPlaceholderText("例如：协作偏好、常用约定")).toBeInTheDocument();
    expect(screen.getByPlaceholderText("记录需要跨会话长期保留的稳定信息")).toBeInTheDocument();
  });
});

function settingsStore(): RootStore {
  const store = new RootStore();
  store.settings.is_open = true;
  store.settings.page = "models";
  store.settings.status = {
    config_path: "/private/runtime/config.toml",
    revision: "revision-1",
    state: "ready",
    schema_version: 1,
    default_model: "primary",
    auxiliary_vision_model: null,
    issues: [],
  };
  store.settings.model_catalog = modelCatalog();
  return store;
}

function modelCatalog(): ModelCatalogSnapshot {
  return {
    revision: "fixture",
    entries: [
      catalogEntry("deepseek", "DeepSeek", ["deepseek-v4-flash", "deepseek-v4-pro"]),
      catalogEntry("zhipu", "智谱 GLM", ["glm-5.2", "glm-5v-turbo"]),
      catalogEntry("dashscope", "阿里云百炼（Qwen）", ["qwen3.8-max"]),
      catalogEntry("openai", "OpenAI", ["gpt-5.6"]),
      catalogEntry("moonshot", "Moonshot（Kimi）", ["kimi-k3"]),
    ],
  };
}

function catalogEntry(provider: string, provider_label: string, model_ids: string[]) {
  return {
    provider,
    provider_label,
    protocol: "openai_chat_completions",
    protocol_label: "Chat Completions（OpenAI Compatible）",
    model_ids,
  };
}

function model(model_key: string, is_default: boolean): ModelConfiguration {
  return {
    model_key,
    display_name: model_key,
    protocol: "openai_chat_completions",
    provider: "fixture",
    endpoint: "https://api.example.test/v1",
    model: `${model_key}-model`,
    context_window_tokens: 8_192,
    max_output_tokens: 4_096,
    agent_max_output_tokens: 4_096,
    effective_max_output_tokens: 4_096,
    supports_image_input: false,
    api_key_configured: true,
    origin: "configuration_file",
    editable: true,
    deletable: true,
    is_default,
    is_valid: true,
    issues: [],
  };
}

function applicationWithSession(session: SessionSummary): ApplicationSnapshot {
  return {
    runtime_lifecycle: "running",
    configuration: {
      config_path: null,
      revision: "revision-1",
      state: "ready",
      schema_version: 1,
      default_model: "primary",
      auxiliary_vision_model: null,
      issues: [],
    },
    models: [],
    workspaces: [],
    active_sessions: [session],
    archived_sessions: [],
    capabilities: {
      conversation_paging: true,
      tool_detail: true,
      queue_control: true,
      approval_queue: true,
      child_task_view: true,
      conversation_search: true,
    },
  };
}

function modelSession(active_run_id: string | null): SessionSummary {
  return {
    session_id: "session-1",
    title: "模型引用会话",
    model_key: "secondary",
    lifecycle: "active",
    current_variant: "build",
    approval_mode: "ask",
    workspace_id: null,
    active_run_id,
    message_count: 1,
    queued_input_count: 0,
    resume_required: false,
    created_at_ms: 1,
    updated_at_ms: 1,
    archived_at_ms: null,
    is_pinned: false,
    title_origin: "user",
    pending_approval_count: 0,
    active_child_count: 0,
    active_run_status: active_run_id ? "running" : null,
  };
}

function permissionDocument(
  scope: PermissionDocumentSnapshot["scope"],
  editable: boolean,
): PermissionDocumentSnapshot {
  return {
    scope,
    revision: { type: "content", payload: { value: "revision-1" } },
    status: "ready",
    schema_version: 1,
    rules: [],
    diagnostics: [],
    editable,
  };
}

const memoryCapabilities: MemoryCapabilities = {
  max_persona_bytes: 16_384,
  max_pinned_entries: 256,
  max_pinned_category_bytes: 128,
  max_pinned_content_bytes: 16_384,
  max_attributes_per_entry: 32,
  max_attribute_key_bytes: 128,
  max_attribute_string_bytes: 4_096,
};

function personaSnapshot(): PersonaSnapshot {
  return {
    enabled: true,
    content: "结论优先",
    revision: 1,
    updated_at_ms: 1,
  };
}

function pinnedCollection(): PinnedMemoryCollectionSnapshot {
  return {
    revision: 0,
    items: [],
    capabilities: memoryCapabilities,
  };
}

function renderDialog(store: RootStore) {
  return render(
    <RootStoreProvider store={store}>
      <SettingsDialog />
    </RootStoreProvider>,
  );
}
