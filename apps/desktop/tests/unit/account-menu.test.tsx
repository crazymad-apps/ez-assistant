import { act, fireEvent, render, screen } from "@testing-library/react";
import { runInAction } from "mobx";
import { describe, expect, it, vi } from "vitest";
import { AccountMenu } from "../../src/features/runtime-access/AccountMenu";
import { ApplicationConnectionStore } from "../../src/features/runtime-access/ApplicationConnectionStore";
import { RootStore } from "../../src/stores/RootStore";

describe("AccountMenu", () => {
  it("allows opening settings while workspace resources are still restoring", () => {
    const root = new RootStore();
    const connection = new ApplicationConnectionStore(root);
    const open = vi.spyOn(root.settings, "open").mockImplementation(() => undefined);
    runInAction(() => {
      connection.current = root;
      connection.phase = "workspace";
      connection.pending = true;
      root.local_connection.state = "connected";
    });
    const view = render(<AccountMenu connection={connection} sidebar />);
    fireEvent.click(screen.getByRole("button", { name: "账号与 Runtime" }));
    expect(screen.getByRole("menuitem", { name: "设置" })).toBeEnabled();
    expect(screen.getByRole("menuitem", { name: "退出登录" })).toBeDisabled();
    fireEvent.click(screen.getByRole("menuitem", { name: "设置" }));
    expect(open).toHaveBeenCalledOnce();
    act(() => runInAction(() => { connection.phase = "entry"; }));
    expect(screen.getByRole("button", { name: "账号与 Runtime" })).toBeDisabled();
    view.unmount();
    open.mockRestore();
    connection.dispose();
  });

  it("keeps feedback available for remote sessions while the local login is connected", () => {
    const root = new RootStore();
    const connection = new ApplicationConnectionStore(root);
    const show = vi.spyOn(root.feedback_center, "show").mockImplementation(() => undefined);
    runInAction(() => {
      root.local_connection.state = "connected";
      root.navigation.selectSession("remote-session", true, "11111111-1111-4111-8111-111111111111");
      connection.current = root;
      connection.phase = "workspace";
      connection.session = {
        token: null, expires_at_ms: 0, instance_id: "fixture", mode: "enterprise", kind: "user",
        identity: { center_id: "fixture", user_id: 1, username: "tester", display_name: "测试用户" },
        login_context: "fixture",
      };
    });
    const view = render(<AccountMenu connection={connection} />);
    fireEvent.click(screen.getByRole("button", { name: "账号与 Runtime" }));
    expect(screen.getByRole("menuitem", { name: "反馈中心" })).toBeEnabled();
    fireEvent.click(screen.getByRole("menuitem", { name: "反馈中心" }));
    expect(show).toHaveBeenCalledOnce();
    act(() => runInAction(() => { root.local_connection.state = "disconnected"; }));
    fireEvent.click(screen.getByRole("button", { name: "账号与 Runtime" }));
    expect(screen.getByRole("menuitem", { name: "反馈中心" })).toBeDisabled();
    view.unmount();
    show.mockRestore();
    connection.dispose();
  });

  it("renders workspace connection status without a RootStoreProvider", () => {
    const root = new RootStore();
    const connection = new ApplicationConnectionStore(root);
    runInAction(() => {
      root.connection.state = "connected";
      connection.current = root;
      connection.phase = "workspace";
      connection.session = {
        token: null,
        expires_at_ms: 0,
        instance_id: "fixture",
        mode: "personal",
        kind: "user",
        identity: null,
        login_context: "fixture",
      };
    });

    const view = render(<AccountMenu connection={connection} />);
    expect(screen.getByRole("button", { name: "账号与 Runtime" })).toBeVisible();
    expect(screen.getByRole("button", { name: "账号与 Runtime" }).querySelector('[data-icon="settings"]')).not.toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "账号与 Runtime" }));
    expect(screen.getByRole("menuitem", { name: "设置" })).toBeEnabled();
    expect(screen.getByText("Runtime 已连接")).toBeVisible();
    expect(screen.queryByText("运行时已连接")).not.toBeInTheDocument();
    view.unmount();
    connection.dispose();
  });

  it("sizes the sidebar menu to the sidebar content width", () => {
    const root = new RootStore();
    const connection = new ApplicationConnectionStore(root);
    runInAction(() => {
      root.connection.state = "connected";
      root.navigation.left_sidebar_width = 360;
      connection.phase = "workspace";
      connection.current = root;
    });

    const view = render(<AccountMenu connection={connection} sidebar />);
    fireEvent.click(screen.getByRole("button", { name: "账号与 Runtime" }));
    expect(screen.getByRole("menuitem", { name: "设置" })).toBeEnabled();
    expect(screen.getByRole("menu", { name: "账号菜单" })).toHaveStyle({
      width: "calc(min(360px, 100vw - 56px) - 24px)",
    });
    view.unmount();
    connection.dispose();
  });
});
