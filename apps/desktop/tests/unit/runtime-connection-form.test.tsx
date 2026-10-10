import { fireEvent, render, screen } from "@testing-library/react";
import { runInAction } from "mobx";
import { describe, expect, it, vi } from "vitest";
import { ApplicationConnectionStore } from "../../src/features/runtime-access/ApplicationConnectionStore";
import { RuntimeConnectionForm } from "../../src/features/runtime-access/RuntimeConnectionForm";
import { RootStore } from "../../src/stores/RootStore";

vi.mock("@tauri-apps/api/core", async (importOriginal) => ({
  ...await importOriginal<typeof import("@tauri-apps/api/core")>(), isTauri: () => true,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: async () => () => undefined }));

vi.mock("../../src/native-bridge/desktopLifecycle", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../src/native-bridge/desktopLifecycle")>(),
  getDesktopPlatform: async () => "macos",
}));

describe("Runtime switch form", () => {
  it("offers a clear update action without connecting or upgrading on render", () => {
    const connection = new ApplicationConnectionStore(new RootStore());
    runInAction(() => {
      connection.local_state = "upgrade_required";
      connection.mode = "enterprise";
      connection.local_error = "更新将重启 Runtime 并中断其正在执行的任务，随后自动备份和升级数据。";
    });
    const upgrade = vi.spyOn(connection, "upgradeLocal").mockReturnValue(new Promise(() => undefined));
    const connect = vi.spyOn(connection, "connectDesktop").mockResolvedValue();
    const view = render(<RuntimeConnectionForm connection={connection} />);
    expect(screen.getByText("本机 Runtime 待更新")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("中断其正在执行的任务");
    expect(screen.queryByRole("button", { name: "重试启动" })).not.toBeInTheDocument();
    expect(screen.queryByLabelText("企业账号")).not.toBeInTheDocument();
    expect(upgrade).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "更新并重启" }));
    expect(upgrade).toHaveBeenCalledOnce();
    expect(connect).not.toHaveBeenCalled();
    view.unmount();
    connection.dispose();
  });

  it.each(["local", "remote"] as const)("allows an enterprise account switch on the current %s Host without showing personal information", async (target) => {
    const root = new RootStore();
    const connection = new ApplicationConnectionStore(root);
    runInAction(() => {
      root.connection.state = "connected";
      root.connection.address = "http://localhost:17241";
      connection.current = root;
      connection.local_state = "ready";
      connection.mode = "enterprise";
      connection.target = target;
      connection.connected_target = target;
      connection.address = root.connection.address;
      connection.phase = "workspace";
      connection.session = { token: null, expires_at_ms: 0, instance_id: "fixture", mode: "enterprise", kind: "user", identity: null, login_context: "fixture" };
    });
    vi.spyOn(connection, "account_label", "get").mockReturnValue("不应显示的姓名");
    const connect = vi.spyOn(connection, "connectDesktop").mockResolvedValue();
    const view = render(<RuntimeConnectionForm connection={connection} settings />);
    const button = screen.getByRole("button", { name: "确认切换" });
    expect(button).toBeDisabled();
    expect(screen.queryByText("不应显示的姓名")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "当前连接" })).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("企业账号"), { target: { value: "another-user" } });
    fireEvent.change(screen.getByLabelText("账号密码"), { target: { value: "fixture-password" } });
    expect(button).toBeEnabled();
    fireEvent.click(button);
    expect(connect).toHaveBeenCalledWith(target === "local" ? null : "http://localhost:17241", "fixture-password", false, "another-user");
    view.unmount();
    connection.dispose();
  });
  it.each([false, true])("keeps the update gate simple and shows an action only with an available update (%s)", (available) => {
    const connection = new ApplicationConnectionStore(new RootStore());
    runInAction(() => { connection.mode = "enterprise"; connection.local_state = "ready"; connection.host_update_required = true; });
    connection.updates.receive({ ...connection.updates.snapshot, revision: 1, available, required: true, phase: available ? "available" : "idle" });
    const connect = vi.spyOn(connection, "connectDesktop").mockResolvedValue();
    const view = render(<RuntimeConnectionForm connection={connection} />);
    expect(screen.getByText("需要更新后继续使用")).toBeInTheDocument();
    expect(screen.queryByLabelText("企业账号")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("账号密码")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /进入项目/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "检查更新" })).not.toBeInTheDocument();
    if (available) {
      fireEvent.click(screen.getByRole("button", { name: "更新并重启" }));
      expect(connection.updates.open).toBe(true);
      connection.updates.hide();
      expect(connection.update_required).toBe(true);
    } else {
      expect(screen.queryByRole("button", { name: "更新并重启" })).not.toBeInTheDocument();
      expect(screen.getByText("暂时无法更新，请稍后重试。")).toBeInTheDocument();
    }
    fireEvent.submit(view.container.querySelector("form")!);
    expect(connect).not.toHaveBeenCalled();
    view.unmount(); connection.dispose();
  });

});
