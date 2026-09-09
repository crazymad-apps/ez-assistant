import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SettingsDialog } from "../../src/features/settings/SettingsDialog";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";
import { discoveredModel, modelParameters, modelProvider, modelSelection } from "../support/modelManagement";
import { compileTokenDraft, sameParameters, tokenDraft } from "../../src/features/settings/SettingsDialog/modelSettingsValues";
import type { ModelConfigurationDetail, ProviderUsage } from "../../src/generated/assistant-protocol";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });
function modelStore() {
  const store = new RootStore();
  store.settings.is_open = true;
  store.settings.page = "models";
  store.settings.providers = [modelProvider];
  vi.spyOn(store.settings, "listProviderModels").mockResolvedValue([discoveredModel()]);
  vi.spyOn(store.settings, "listFixedModels").mockResolvedValue([]);
  return store;
}
function show(store: RootStore) { render(<RootStoreProvider store={store}><SettingsDialog /></RootStoreProvider>); }
function detail(source: "fixed" | "online" = "fixed"): ModelConfigurationDetail {
  return { origin: "online", selection: modelSelection, source, field_sources: {}, template_document: null, template_checked_on: null, parameters: modelParameters(), updated_at_ms: source === "fixed" ? 10 : null };
}
async function openModel(store: RootStore) {
  show(store);
  fireEvent.click(screen.getByRole("button", { name: /测试服务商 https/ }));
  fireEvent.click(await screen.findByRole("button", { name: "配置模型 fixture" }));
  await screen.findByLabelText("上下文窗口（Token）");
}

describe("provider and fixed model settings", () => {
  it("requires successful usage statistics before deletion and permits retry and cancellation", async () => {
    const store = modelStore();
    const usage: ProviderUsage = {
      default_model: true, vision_model: true, session_count: 27, fixed_config_count: 3,
      sessions: [{ session_id: "archived-session", title: "未加载的归档会话" }],
    };
    let finish!: (value: ProviderUsage) => void;
    const query = vi.spyOn(store.settings, "getProviderUsage")
      .mockRejectedValueOnce(new Error("统计暂不可用"))
      .mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    const remove = vi.spyOn(store.settings, "deleteProvider").mockImplementation(async () => {
      store.settings.providers = [];
      store.settings.showNotice("服务商已删除，实际影响 28 个会话。");
      await Promise.resolve();
      return true;
    });
    show(store);
    fireEvent.click(screen.getByRole("button", { name: /测试服务商 https/ }));
    await screen.findByRole("button", { name: "配置模型 fixture" });
    fireEvent.click(screen.getByRole("button", { name: "删除服务商" }));
    const dialog = within(screen.getByRole("dialog", { name: "删除“测试服务商”？" }));
    expect(dialog.getByRole("button", { name: "删除服务商" })).toBeDisabled();
    expect(await dialog.findByRole("alert")).toHaveTextContent("统计暂不可用");
    expect(dialog.getByRole("button", { name: "取消" })).toBeEnabled();
    fireEvent.click(dialog.getByRole("button", { name: "重试" }));
    expect(dialog.getByRole("button", { name: "删除服务商" })).toBeDisabled();
    expect(remove).not.toHaveBeenCalled();
    await act(async () => { finish(usage); });
    expect(dialog.getByText(/27 个显式引用会话/)).toHaveTextContent("3 条固定配置");
    expect(dialog.getByText("未加载的归档会话")).toBeVisible();
    expect(dialog.getByText("当前默认模型使用此服务商。")).toBeVisible();
    expect(dialog.getByText("辅助识图模型使用此服务商。")).toBeVisible();
    expect(dialog.getByText(/另有 26 个会话/)).toBeVisible();
    fireEvent.click(dialog.getByRole("button", { name: "删除服务商" }));
    await waitFor(() => expect(remove).toHaveBeenCalledExactlyOnceWith("provider-1"));
    expect(await screen.findByRole("status")).toHaveTextContent("实际影响 28 个会话");
    expect(query).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("dialog", { name: "删除“测试服务商”？" })).not.toBeInTheDocument();
  });
  it("ignores cancelled usage results when the deletion dialog is reopened", async () => {
    const store = modelStore();
    let finish!: (value: ProviderUsage) => void;
    let signal: AbortSignal | undefined;
    vi.spyOn(store.settings, "getProviderUsage")
      .mockImplementationOnce((_id, request_signal) => {
        signal = request_signal;
        return new Promise((resolve) => { finish = resolve; });
      })
      .mockRejectedValueOnce(new Error("本次读取失败"));
    const remove = vi.spyOn(store.settings, "deleteProvider");
    show(store);
    fireEvent.click(screen.getByRole("button", { name: /测试服务商 https/ }));
    await screen.findByRole("button", { name: "配置模型 fixture" });
    fireEvent.click(screen.getByRole("button", { name: "删除服务商" }));
    fireEvent.click(within(screen.getByRole("dialog", { name: "删除“测试服务商”？" })).getByRole("button", { name: "取消" }));
    expect(signal?.aborted).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "删除服务商" }));
    const dialog = within(screen.getByRole("dialog", { name: "删除“测试服务商”？" }));
    expect(await dialog.findByRole("alert")).toHaveTextContent("本次读取失败");
    await act(async () => { finish({ default_model: false, vision_model: false, session_count: 99, fixed_config_count: 0, sessions: [] }); });
    expect(dialog.getByRole("button", { name: "删除服务商" })).toBeDisabled();
    expect(dialog.queryByText(/99 个显式引用会话/)).not.toBeInTheDocument();
    expect(remove).not.toHaveBeenCalled();
  });
  it("keeps credentials blank and preserves internal and outer navigation drafts", async () => {
    const store = modelStore();
    const save = vi.spyOn(store.settings, "saveProvider").mockResolvedValue(modelProvider);
    show(store);
    fireEvent.click(screen.getByRole("button", { name: /测试服务商 https/ }));
    await screen.findByRole("button", { name: "配置模型 fixture" });
    fireEvent.click(screen.getByRole("button", { name: "编辑" }));
    expect(screen.getByLabelText("API Key")).toHaveValue("");
    fireEvent.change(screen.getByLabelText("服务商名称"), { target: { value: "新名称" } });
    fireEvent.click(screen.getByRole("button", { name: "返回" }));
    fireEvent.click(within(screen.getByRole("dialog", { name: "放弃未保存的修改？" })).getByRole("button", { name: "取消" }));
    expect(screen.getByLabelText("服务商名称")).toHaveValue("新名称");
    fireEvent.click(screen.getByRole("button", { name: "关闭设置" }));
    fireEvent.click(within(screen.getByRole("dialog", { name: "放弃未保存的修改？" })).getByRole("button", { name: "取消" }));
    fireEvent.click(screen.getByRole("button", { name: "保存服务商" }));
    await waitFor(() => expect(save).toHaveBeenCalledWith("provider-1", expect.objectContaining({ display_name: "新名称" }), { mode: "unchanged" }));
    expect(screen.queryByLabelText("API Key")).not.toBeInTheDocument();
  });
  it("opens fixed models independently when online discovery fails", async () => {
    const store = modelStore();
    vi.mocked(store.settings.listProviderModels).mockRejectedValue(new Error("offline"));
    vi.mocked(store.settings.listFixedModels).mockResolvedValue([{ origin: "online", selection: modelSelection, parameters: modelParameters(), updated_at_ms: 10 }]);
    vi.spyOn(store.settings, "getModelConfiguration").mockResolvedValue(detail());
    show(store);
    fireEvent.click(screen.getByRole("button", { name: /测试服务商 https/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent("offline");
    fireEvent.click(await screen.findByRole("button", { name: "编辑固定配置 fixture" }));
    expect(await screen.findByLabelText("上下文窗口（Token）")).toHaveValue("128000");
    expect(store.settings.listFixedModels).toHaveBeenCalledWith("provider-1", 0, expect.any(AbortSignal));
  });
  it("leaves missing online limits blank and saves the complete user draft", async () => {
    const store = modelStore(); const online = detail("online");
    online.parameters.context_window_tokens = { state: "unknown" };
    vi.spyOn(store.settings, "getModelConfiguration").mockResolvedValue(online);
    const save = vi.spyOn(store.settings, "saveModelFixedConfig").mockResolvedValue({ origin: "online", selection: modelSelection, parameters: modelParameters(), updated_at_ms: 11 });
    await openModel(store);
    const context = screen.getByLabelText("上下文窗口（Token）");
    expect(context).toHaveValue("");
    fireEvent.change(context, { target: { value: "128000" } });
    fireEvent.click(screen.getByRole("button", { name: "保存固定配置" }));
    await waitFor(() => expect(save).toHaveBeenCalledWith(modelSelection, modelParameters(), "online"));
    await waitFor(() => expect(screen.getByRole("button", { name: "重置配置" })).toBeEnabled());
    expect(screen.getByRole("button", { name: "重置配置" })).toBeEnabled();
  });
  it("preserves dirty values when a save fails and readback differs", async () => {
    const store = modelStore();
    vi.spyOn(store.settings, "getModelConfiguration").mockResolvedValue(detail());
    const save = vi.spyOn(store.settings, "saveModelFixedConfig").mockResolvedValue(null);
    await openModel(store);
    fireEvent.change(screen.getByLabelText("上下文窗口（Token）"), { target: { value: "64000" } });
    fireEvent.click(screen.getByRole("button", { name: "保存固定配置" }));
    await waitFor(() => expect(store.settings.getModelConfiguration).toHaveBeenCalledTimes(2));
    expect(save).toHaveBeenCalledOnce();
    expect(screen.getByLabelText("上下文窗口（Token）")).toHaveValue("64000");
    fireEvent.click(screen.getByRole("button", { name: "返回" }));
    expect(screen.getByRole("dialog", { name: "放弃未保存的修改？" })).toBeVisible();
  });
  it("confirms a committed save through readback without repeating the write", async () => {
    const store = modelStore(); const committed = detail();
    committed.parameters.context_window_tokens = { state: "known", value: 64000 };
    vi.spyOn(store.settings, "getModelConfiguration").mockResolvedValueOnce(detail()).mockResolvedValueOnce(committed);
    const save = vi.spyOn(store.settings, "saveModelFixedConfig").mockResolvedValue(null);
    await openModel(store);
    fireEvent.change(screen.getByLabelText("上下文窗口（Token）"), { target: { value: "64000" } });
    fireEvent.click(screen.getByRole("button", { name: "保存固定配置" }));
    expect(await screen.findByRole("status")).toHaveTextContent("已重新读取并确认固定配置");
    expect(save).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "返回" }));
    expect(screen.queryByRole("dialog", { name: "放弃未保存的修改？" })).not.toBeInTheDocument();
  });
  it("keeps a successful reset distinct from a subsequent discovery failure", async () => {
    const store = modelStore();
    vi.spyOn(store.settings, "getModelConfiguration").mockResolvedValueOnce(detail()).mockRejectedValueOnce(new Error("online unavailable"));
    const reset = vi.spyOn(store.settings, "resetModelFixedConfig").mockImplementation(async () => { store.settings.showNotice("固定配置已重置。"); return true; });
    await openModel(store);
    fireEvent.click(screen.getByRole("button", { name: "重置配置" }));
    expect(reset).not.toHaveBeenCalled();
    fireEvent.click(within(screen.getByRole("dialog", { name: "重置固定配置？" })).getByRole("button", { name: "重置配置" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("online unavailable");
    expect(screen.getByRole("status")).toHaveTextContent("固定配置已重置");
    expect(screen.queryByLabelText("上下文窗口（Token）")).not.toBeInTheDocument();
    expect(reset).toHaveBeenCalledOnce();
  });
  it("refreshes reference values without overwriting dirty or fixed values", async () => {
    const store = modelStore();
    vi.spyOn(store.settings, "getModelConfiguration").mockResolvedValue(detail());
    await openModel(store);
    const online = discoveredModel(); online.metadata.context_window_tokens = { state: "known", value: 32000 };
    vi.mocked(store.settings.listProviderModels).mockResolvedValue([online]);
    fireEvent.change(screen.getByLabelText("上下文窗口（Token）"), { target: { value: "64000" } });
    fireEvent.click(screen.getByRole("button", { name: "刷新在线参考" }));
    const table = await screen.findByRole("table");
    expect(table).toHaveTextContent("64000"); expect(table).toHaveTextContent("32000");
    expect(screen.getByLabelText("上下文窗口（Token）")).toHaveValue("64000");
    fireEvent.click(screen.getByRole("button", { name: "使用本次在线值" }));
    expect(screen.getByLabelText("上下文窗口（Token）")).toHaveValue("32000");
    expect(screen.getByRole("button", { name: "测试已保存配置" })).toBeDisabled();
  });
  it("requests only the opened provider and ignores results after close", async () => {
    const store = modelStore();
    store.settings.providers = [modelProvider, { ...modelProvider, provider_instance_id: "provider-2", connection: { ...modelProvider.connection, display_name: "另一服务商" } }];
    let finish!: (models: ReturnType<typeof discoveredModel>[]) => void;
    vi.mocked(store.settings.listProviderModels).mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    show(store);
    const trigger = screen.getByRole("button", { name: "默认模型" });
    fireEvent.click(trigger);
    expect(store.settings.listProviderModels).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("menuitem", { name: /另一服务商/ }));
    expect(store.settings.listProviderModels).toHaveBeenCalledWith("provider-2", expect.any(AbortSignal));
    fireEvent.click(trigger); finish([discoveredModel("late-model")]); fireEvent.click(trigger);
    expect(screen.queryByText("late-model")).not.toBeInTheDocument();
    expect(store.settings.listProviderModels).toHaveBeenCalledOnce();
  });
});

describe("fixed parameter draft validation", () => {
  it("rejects fractional, unsafe and contradictory token limits", () => {
    const parameters = modelParameters(); const draft = tokenDraft(parameters);
    for (const value of ["1.5", "0", "-2", "9007199254740992"]) expect(() => compileTokenDraft(parameters, { ...draft, context_window_tokens: value })).toThrow();
    expect(() => compileTokenDraft(parameters, { ...draft, max_output_tokens: "128001" })).toThrow(/不能超过/);
  });
  it("does not equate invalid optional limits with unknown ones", () => {
    const left = modelParameters(); const right = modelParameters(); right.max_input_tokens = { state: "invalid" };
    expect(sameParameters(left, right)).toBe(false);
    expect(sameParameters(left, modelParameters())).toBe(true);
  });
});
