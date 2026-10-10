import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ResourceTerminal } from "../../src/features/resource-workspace/ResourceTerminal";
import type { TerminalController } from "../../src/features/resource-workspace/TerminalController";

afterEach(cleanup);
Element.prototype.scrollIntoView = vi.fn();

it.each([
  { name: "zsh", kind: "bash", program: "bash", label: "Bash" },
  { name: "powershell.exe", kind: "cmd", program: "cmd.exe", label: "CMD" },
])("shows $name Host shells and confirms switching away from and back to default", async ({ name, kind, program, label }) => {
  const controller = {
    status: "running", ready: true, terminal_id: "pty", selected_shell: null, shell_kind: "zsh", shell_name: name,
    shell_catalog: [{ shell: null, name }, { shell: kind, name: program }],
    mount: vi.fn(), unmount: vi.fn(), fit: vi.fn(), start: vi.fn(), focus: vi.fn(), selectShell: vi.fn(),
  };
  const user = userEvent.setup();
  const view = render(<ResourceTerminal controller={controller as unknown as TerminalController} active />);
  const trigger = screen.getByRole("button", { name: "终端 Shell 类型" });
  expect(trigger).toHaveTextContent(`系统默认 Shell（${name}）`);
  await user.click(trigger);
  expect(screen.getAllByRole("option")).toHaveLength(2);
  await user.click(screen.getByRole("option", { name: label }));
  expect(screen.getByRole("dialog", { name: "切换终端 Shell" })).toBeVisible();
  expect(controller.selectShell).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "取消" }));
  expect(controller.selectShell).not.toHaveBeenCalled();
  await user.click(trigger);
  await user.click(screen.getByRole("option", { name: label }));
  await user.click(screen.getByRole("button", { name: "切换 Shell" }));
  expect(controller.selectShell).toHaveBeenLastCalledWith(kind);

  const switched = { ...controller, selected_shell: kind, shell_kind: kind, shell_name: program };
  view.rerender(<ResourceTerminal controller={switched as unknown as TerminalController} active />);
  expect(trigger).toHaveTextContent(label);
  await user.click(trigger);
  await user.click(screen.getByRole("option", { name: `系统默认 Shell（${name}）` }));
  await user.click(screen.getByRole("button", { name: "切换 Shell" }));
  expect(controller.selectShell).toHaveBeenLastCalledWith(null);
});
