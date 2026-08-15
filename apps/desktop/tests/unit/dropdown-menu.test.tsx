import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../src/components/DropdownMenu";

afterEach(cleanup);

function ExampleMenu() {
  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger aria-label="打开操作">操作</DropdownMenuTrigger>
        <DropdownMenuContent aria-label="操作菜单">
          <DropdownMenuItem>菜单项</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <button type="button">页面空白操作</button>
    </>
  );
}

describe("DropdownMenu", () => {
  it("closes when the user clicks elsewhere on the page", async () => {
    const user = userEvent.setup();
    render(<ExampleMenu />);

    await user.click(screen.getByRole("button", { name: "打开操作" }));
    expect(screen.getByRole("menu", { name: "操作菜单" })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "页面空白操作" }));
    expect(screen.queryByRole("menu", { name: "操作菜单" })).not.toBeInTheDocument();
  });

  it("does not focus the first item when opened with a pointer", async () => {
    const user = userEvent.setup();
    render(<ExampleMenu />);

    await user.click(screen.getByRole("button", { name: "打开操作" }));

    expect(screen.getByRole("menuitem", { name: "菜单项" })).not.toHaveFocus();
  });

  it("focuses the first item when opened from the keyboard", async () => {
    const user = userEvent.setup();
    render(<ExampleMenu />);
    const trigger = screen.getByRole("button", { name: "打开操作" });
    trigger.focus();

    await user.keyboard("{Enter}");

    expect(screen.getByRole("menuitem", { name: "菜单项" })).toHaveFocus();
  });

  it("closes with Escape and restores focus to the trigger", async () => {
    const user = userEvent.setup();
    render(<ExampleMenu />);
    const trigger = screen.getByRole("button", { name: "打开操作" });

    await user.click(trigger);
    await user.keyboard("{Escape}");

    expect(screen.queryByRole("menu", { name: "操作菜单" })).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });
});
