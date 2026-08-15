import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SettingsDialog } from "../../src/features/settings/SettingsDialog";
import type {
  ModelConfiguration,
  PermissionDocumentSnapshot,
} from "../../src/generated/assistant-protocol";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("SettingsDialog model management", () => {
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
    const provider = screen.getByRole("button", { name: "选择供应商方言" });
    fireEvent.click(provider);
    expect(within(screen.getByRole("listbox", { name: "选择供应商方言" })).getAllByRole("option").map((option) => option.textContent)).toEqual([
      "DeepSeek",
      "OpenAI Compatible",
    ]);
    fireEvent.click(within(screen.getByRole("listbox", { name: "选择供应商方言" })).getByRole("option", { name: "DeepSeek" }));
    expect(screen.getByLabelText("模型 Key")).toHaveAttribute(
      "placeholder",
      "应用内唯一标识，例如：deepseek-v4-pro",
    );
    expect(screen.getByLabelText("模型 ID")).toHaveAttribute(
      "placeholder",
      "供应商接口使用的模型名，例如：deepseek-chat",
    );
    fireEvent.change(screen.getByLabelText("显示名称"), { target: { value: "Fixture Pro" } });
    fireEvent.change(screen.getByLabelText("模型 Key"), { target: { value: "fixture-pro" } });
    fireEvent.change(screen.getByLabelText("模型 ID"), { target: { value: "fixture-pro-model" } });
    fireEvent.change(screen.getByLabelText("Endpoint"), { target: { value: "https://api.example.test/v1" } });
    fireEvent.change(screen.getByLabelText("API Key"), { target: { value: "form-secret" } });
    fireEvent.click(screen.getByLabelText("设为默认模型"));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(create).toHaveBeenCalledWith(expect.objectContaining({
      model_key: "fixture-pro",
      protocol: "chat_completions",
      provider: "deepseek",
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
    fireEvent.change(screen.getByLabelText("模型 ID"), { target: { value: "qwen3.8-max" } });
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

    expect(screen.getByRole("button", { name: "当前会话" })).toHaveTextContent(/^当前会话$/);
    expect(screen.getByRole("button", { name: "Workspace" })).toHaveTextContent(/^Workspace$/);
    expect(screen.getByRole("button", { name: "全局" })).toHaveTextContent(/^全局$/);

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
    issues: [],
  };
  return store;
}

function model(model_key: string, is_default: boolean): ModelConfiguration {
  return {
    model_key,
    display_name: model_key,
    protocol: "chat_completions",
    provider: "fixture",
    endpoint: "https://api.example.test/v1",
    model: `${model_key}-model`,
    context_window_tokens: 8_192,
    max_output_tokens: 4_096,
    agent_max_output_tokens: 4_096,
    effective_max_output_tokens: 4_096,
    api_key_configured: true,
    origin: "configuration_file",
    editable: true,
    deletable: true,
    is_default,
    is_valid: true,
    issues: [],
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

function renderDialog(store: RootStore) {
  return render(
    <RootStoreProvider store={store}>
      <SettingsDialog />
    </RootStoreProvider>,
  );
}
