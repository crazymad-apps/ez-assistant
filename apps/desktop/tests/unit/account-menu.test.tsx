import { act, fireEvent, render, screen, within } from "@testing-library/react";
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
        identity: { center_id: "fixture", organization_id: "org-fixture", organization_name: "测试组织", user_id: 1, username: "tester", display_name: "测试用户" },
        login_context: "fixture",
      };
    });
    const view = render(<AccountMenu connection={connection} />);
    const trigger = screen.getByRole("button", { name: "账号与 Runtime" });
    fireEvent.click(within(trigger).getByText("测试用户", { exact: true }));
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

  it("expands the organization options inside the account menu and closes it on selection", () => {
    const root = new RootStore();
    const connection = new ApplicationConnectionStore(root);
    const read = vi.spyOn(connection, "openOrganizationSwitch").mockImplementation(async () => {
      runInAction(() => {
        connection.organization_switch = {
          phase: "options",
          organizations: [{ id: "other-org", name: "另一个组织" }],
        };
      });
    });
    const preview = vi.spyOn(connection, "previewOrganizationSwitch").mockResolvedValue();
    runInAction(() => {
      connection.current = root;
      connection.phase = "workspace";
      connection.session = {
        token: null, expires_at_ms: 0, instance_id: "fixture", mode: "enterprise", kind: "user",
        identity: { center_id: "fixture", organization_id: "org-fixture", organization_name: "测试组织", user_id: 1, username: "tester", display_name: "测试用户" },
        login_context: "fixture",
      };
    });
    const view = render(<AccountMenu connection={connection} />);
    const trigger = screen.getByRole("button", { name: "账号与 Runtime" });
    expect(screen.queryByText("测试组织")).not.toBeInTheDocument();
    fireEvent.click(trigger);
    const menu = screen.getByRole("menu", { name: "账号菜单" });
    expect(read).not.toHaveBeenCalled();
    const organization = within(menu).getByRole("menuitem", { name: "切换组织，当前组织：测试组织" });
    fireEvent.click(organization);
    expect(organization).toHaveAttribute("aria-expanded", "true");
    const choices = within(menu).getByRole("group", { name: "可切换组织" });
    expect(organization).toHaveTextContent("测试组织");
    expect(within(choices).getByRole("menuitem", { name: "另一个组织" })).toBeInTheDocument();
    expect(within(choices).queryByText("测试组织")).not.toBeInTheDocument();
    fireEvent.click(organization);
    expect(organization).toHaveAttribute("aria-expanded", "false");
    expect(within(menu).queryByRole("group", { name: "可切换组织" })).not.toBeInTheDocument();
    organization.focus();
    fireEvent.keyDown(organization, { key: "ArrowDown" });
    expect(within(menu).getByRole("menuitem", { name: "设置" })).toHaveFocus();
    fireEvent.click(organization);
    fireEvent.click(within(menu).getByRole("menuitem", { name: "另一个组织" }));
    expect(preview).toHaveBeenCalledWith("other-org");
    expect(screen.queryByRole("menu", { name: "账号菜单" })).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
    view.unmount();
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
