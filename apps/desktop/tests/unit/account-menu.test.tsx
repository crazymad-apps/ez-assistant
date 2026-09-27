import { fireEvent, render, screen } from "@testing-library/react";
import { runInAction } from "mobx";
import { describe, expect, it } from "vitest";
import { AccountMenu } from "../../src/features/runtime-access/AccountMenu";
import { ApplicationConnectionStore } from "../../src/features/runtime-access/ApplicationConnectionStore";
import { RootStore } from "../../src/stores/RootStore";

describe("AccountMenu", () => {
  it("renders on the entry page without a RootStoreProvider", () => {
    const root = new RootStore();
    const connection = new ApplicationConnectionStore(root);
    runInAction(() => {
      root.connection.state = "connected";
      connection.current = root;
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
    expect(screen.getByRole("button", { name: "账号与 Runtime" }).querySelector('[data-icon="settings"]')).toBeVisible();
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
      connection.current = root;
    });

    const view = render(<AccountMenu connection={connection} sidebar />);
    fireEvent.click(screen.getByRole("button", { name: "账号与 Runtime" }));
    expect(screen.getByRole("menu", { name: "账号菜单" })).toHaveStyle({
      width: "calc(min(360px, 100vw - 56px) - 24px)",
    });
    view.unmount();
    connection.dispose();
  });
});
