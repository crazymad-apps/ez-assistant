import { useState } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ModelPicker } from "../../src/features/settings/ModelPicker";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";
import { discoveredModel, modelProvider, modelSelection } from "../support/modelManagement";
import type { ModelSelection } from "@ez-assistant/protocol";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

function Picker(props: Readonly<{ select: (selection: ModelSelection | null) => Promise<boolean> }>) {
  const [open, setOpen] = useState(false);
  return <ModelPicker providers={[modelProvider]} selection={modelSelection} title="选择模型" label="fixture"
    open={open} onOpenChange={setOpen} onSelect={props.select} trigger_class_name="" follow_default />;
}
function show(select: (selection: ModelSelection | null) => Promise<boolean>) {
  const store = new RootStore();
  vi.spyOn(store.settings, "listProviderModels").mockResolvedValue([discoveredModel("alternate")]);
  render(<RootStoreProvider store={store}><Picker select={select} /></RootStoreProvider>);
  return store;
}

describe("model picker interaction ownership", () => {
  it("serializes follow-default selection and does not close a reopened menu on late success", async () => {
    let finish!: (ok: boolean) => void;
    const select = vi.fn(() => new Promise<boolean>((resolve) => { finish = resolve; }));
    show(select);
    const trigger = screen.getByRole("button", { name: "选择模型" });
    fireEvent.click(trigger);
    const follow = screen.getByRole("menuitemradio", { name: "默认模型" });
    fireEvent.click(follow);
    fireEvent.click(follow);
    expect(select).toHaveBeenCalledExactlyOnceWith(null);
    expect(follow).toBeDisabled();
    fireEvent.click(trigger);
    fireEvent.click(trigger);
    await act(async () => { finish(true); });
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("menuitemradio", { name: "默认模型" })).toBeEnabled();
  });

  it("keeps the menu open after rejection and permits retry", async () => {
    const select = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce(true);
    show(select);
    fireEvent.click(screen.getByRole("button", { name: "选择模型" }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: "默认模型" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("未能保存设置");
    fireEvent.click(screen.getByRole("menuitemradio", { name: "默认模型" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "选择模型" })).toHaveAttribute("aria-expanded", "false"));
    expect(select).toHaveBeenCalledTimes(2);
  });

  it("makes primary actions, provider categories and management reachable by arrow keys", async () => {
    show(vi.fn().mockResolvedValue(true));
    fireEvent.click(screen.getByRole("button", { name: "选择模型" }));
    const follow = screen.getByRole("menuitemradio", { name: "默认模型" });
    await waitFor(() => expect(follow).toHaveFocus());
    fireEvent.keyDown(follow, { key: "ArrowDown" });
    const provider = screen.getByRole("menuitem", { name: /测试服务商/ });
    expect(provider).toHaveFocus();
    fireEvent.keyDown(provider, { key: "ArrowDown" });
    const manage = screen.getByRole("menuitem", { name: "管理服务商" });
    expect(manage).toHaveFocus();
    fireEvent.keyDown(manage, { key: "Home" });
    expect(follow).toHaveFocus();
    fireEvent.keyDown(follow, { key: "End" });
    expect(manage).toHaveFocus();
    fireEvent.keyDown(manage, { key: "Escape" });
    expect(screen.getByRole("button", { name: "选择模型" })).toHaveAttribute("aria-expanded", "false");
  });
});
