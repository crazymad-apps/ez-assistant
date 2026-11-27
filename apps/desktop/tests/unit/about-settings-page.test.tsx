import * as nativeCore from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AppMessageProvider } from "../../src/components/AppMessage";
import { ApplicationConnectionContext } from "../../src/features/runtime-access/ApplicationConnectionContext";
import type { ApplicationConnectionStore } from "../../src/features/runtime-access/ApplicationConnectionStore";
import { SettingsDialog } from "../../src/features/settings/SettingsDialog";
import { CURRENT_VERSION_FEATURES } from "../../src/features/settings/SettingsDialog/AboutSettingsPage/aboutContent";
import * as desktopLifecycle from "../../src/native-bridge/desktopLifecycle";
import * as clipboard from "../../src/platform/clipboard";
import { AppearanceProvider } from "../../src/stores/AppearanceContext";
import { AppearanceStore } from "../../src/stores/AppearanceStore";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));

vi.mock("@tauri-apps/api/core", async () => ({
  ...(await vi.importActual<typeof import("@tauri-apps/api/core")>("@tauri-apps/api/core")),
  isTauri: vi.fn(() => false),
}));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.mocked(nativeCore.isTauri).mockReturnValue(false);
});

function createStore(): RootStore {
  const store = new RootStore();
  store.settings.is_open = true;
  store.settings.page = "runtime";
  store.local_connection.markConnected(
    "instance-about-test",
    {
      runtime_version: __APP_VERSION__,
      min_compatible_version: __APP_VERSION__,
      mode: "personal",
      platform: "macos",
      architecture: "aarch64",
      rg_on_path: true,
      max_command_bytes: 1024 * 1024,
      max_attachment_bytes: null,
      sse: true,
      streaming_upload: true,
      features: [],
    },
    "http://127.0.0.1:7240",
  );
  return store;
}

function renderSettings(
  store: RootStore,
  app_connection: ApplicationConnectionStore | null = null,
) {
  const appearance = new AppearanceStore();
  return render(
    <AppearanceProvider store={appearance}>
      <AppMessageProvider>
        <ApplicationConnectionContext.Provider value={app_connection}>
          <RootStoreProvider store={store}>
            <SettingsDialog />
          </RootStoreProvider>
        </ApplicationConnectionContext.Provider>
      </AppMessageProvider>
    </AppearanceProvider>,
  );
}

describe("SettingsDialog About tab", () => {
  it("opens on the About page by default when settings.open() is called without arguments", () => {
    const store = new RootStore();
    vi.spyOn(store.settings, "load").mockResolvedValue(undefined);
    expect(store.settings.page).toBe("about");
    store.settings.selectPage("runtime");
    store.settings.open();
    expect(store.settings.is_open).toBe(true);
    expect(store.settings.page).toBe("about");
    store.dispose();
  });

  it("places the About tab after Agents and renders current version and features", async () => {
    vi.mocked(nativeCore.isTauri).mockReturnValue(true);
    vi.spyOn(desktopLifecycle, "getDesktopPlatform").mockResolvedValue("macos");
    const store = createStore();
    renderSettings(store);
    const user = userEvent.setup();

    const navigation = screen.getByRole("navigation", { name: "设置页面" });
    const nav_buttons = within(navigation).getAllByRole("button");
    const labels = nav_buttons.map((button) => button.textContent?.trim());
    expect(labels.at(-2)).toBe("智能体");
    expect(labels.at(-1)).toBe("关于");

    const about_tab = within(navigation).getByRole("button", { name: "关于" });
    expect(about_tab.querySelector('[data-icon="info"]')).toBeInTheDocument();

    await user.click(about_tab);
    expect(about_tab).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("heading", { name: "关于" })).toBeVisible();

    const version_region = screen.getByRole("region", { name: "版本信息" });
    expect(version_region).toHaveTextContent("EZ Assistant");
    expect(version_region).toHaveTextContent(`客户端 v${__APP_VERSION__}`);
    expect(version_region).toHaveTextContent(`Runtime v${__APP_VERSION__}`);

    const feature_region = screen.getByRole("region", { name: "当前版本特性" });
    for (const feature of CURRENT_VERSION_FEATURES) {
      expect(within(feature_region).getByText(feature.title)).toBeVisible();
    }

    store.dispose();
  });

  it("copies structured environment summary without leaking secrets", async () => {
    vi.mocked(nativeCore.isTauri).mockReturnValue(true);
    vi.spyOn(desktopLifecycle, "getDesktopPlatform").mockResolvedValue("macos");
    const copy_spy = vi.spyOn(clipboard, "copyText").mockResolvedValue(undefined);
    const store = createStore();
    store.settings.page = "about";
    renderSettings(store);
    const user = userEvent.setup();

    await waitFor(() => expect(desktopLifecycle.getDesktopPlatform).toHaveBeenCalled());
    await user.click(screen.getByRole("button", { name: "复制环境" }));

    await waitFor(() => expect(copy_spy).toHaveBeenCalledTimes(1));
    const copied_text = copy_spy.mock.calls[0]?.[0] ?? "";
    expect(copied_text).toContain("应用: EZ Assistant");
    expect(copied_text).toContain("客户端: Desktop");
    expect(copied_text).toContain(`客户端版本: v${__APP_VERSION__}`);
    expect(copied_text).toContain("客户端系统: macOS");
    expect(copied_text).toContain(`Runtime 版本: v${__APP_VERSION__} (已连接)`);
    expect(copied_text).toContain("Host 平台: macOS · arm64");
    expect(screen.getByRole("button", { name: "已复制" })).toBeVisible();

    store.dispose();
  });

  it("navigates across changelog, app features, licenses, and runtime diagnostics", async () => {
    const store = createStore();
    store.settings.page = "about";
    renderSettings(store);
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: /更新日志/ }));
    expect(screen.getByRole("heading", { name: "更新日志" })).toBeVisible();
    expect(screen.getByText("当前版本")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "返回关于" }));
    expect(screen.getByRole("heading", { name: "关于" })).toBeVisible();

    await user.click(screen.getByRole("button", { name: /App 功能/ }));
    expect(screen.getByRole("heading", { name: "App 功能" })).toBeVisible();
    expect(screen.getByText("多会话与工作空间")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "返回关于" }));

    await user.click(screen.getByRole("button", { name: /开源许可与致谢/ }));
    expect(screen.getByRole("heading", { name: "开源许可与致谢" })).toBeVisible();
    expect(screen.getByText("Tauri 2")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "返回关于" }));

    await user.click(screen.getByRole("button", { name: /状态与诊断/ }));
    expect(screen.getByRole("heading", { name: "状态与诊断" })).toBeVisible();

    store.dispose();
  });

  it("exposes enterprise feedback and desktop update triggers when available", async () => {
    vi.mocked(nativeCore.isTauri).mockReturnValue(true);
    vi.spyOn(desktopLifecycle, "getDesktopPlatform").mockResolvedValue("macos");
    const store = createStore();
    store.settings.page = "about";
    const feedback_show = vi.spyOn(store.feedback_center, "show").mockImplementation(() => undefined);
    const update_show = vi.fn();
    const fake_connection = {
      session: {
        identity: {
          organization_id: "org-1",
          organization_name: "Test Org",
        },
      },
      updates: {
        visible: true,
        show: update_show,
      },
    } as unknown as ApplicationConnectionStore;

    renderSettings(store, fake_connection);
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: "发现新版本" }));
    expect(update_show).toHaveBeenCalledTimes(1);

    await user.click(screen.getByRole("button", { name: /发送反馈/ }));
    expect(feedback_show).toHaveBeenCalledTimes(1);
    expect(store.settings.is_open).toBe(false);

    store.dispose();
  });
});
