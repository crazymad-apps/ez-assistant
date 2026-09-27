import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AppMessageProvider, useAppMessage } from "../../src/components/AppMessage";
import { SettingsMessages } from "../../src/features/settings/SettingsDialog/SettingsMessages";
import type { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";

afterEach(() => { cleanup(); vi.useRealTimers(); });

function Trigger() {
  const show_message = useAppMessage();
  return <button onClick={() => show_message("插件设置已保存。")} type="button">显示提示</button>;
}

it("shows a temporary message above the app without adding an inline alert", () => {
  vi.useFakeTimers();
  render(<AppMessageProvider><Trigger /></AppMessageProvider>);
  fireEvent.click(screen.getByRole("button", { name: "显示提示" }));
  const message = screen.getByRole("status");
  expect(message).toHaveTextContent("插件设置已保存。");
  expect(message.parentElement).toBe(document.body.lastElementChild);
  act(() => vi.advanceTimersByTime(3500));
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
});

it("moves a settings success notice into the temporary message layer", () => {
  const messages = { error_message: null, notice_message: "配置已保存。", dismissNotice: vi.fn() };
  const store = { settings: messages } as unknown as RootStore;
  render(<AppMessageProvider><RootStoreProvider store={store}><SettingsMessages messages={messages} /></RootStoreProvider></AppMessageProvider>);
  expect(screen.getByRole("status")).toHaveTextContent("配置已保存。");
  expect(messages.dismissNotice).toHaveBeenCalledWith("配置已保存。");
  expect(screen.queryByText("配置已保存。", { selector: "p" })).not.toBeInTheDocument();
});
