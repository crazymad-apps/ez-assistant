import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { WorkspaceTransition } from "../../src/features/runtime-access/WorkspaceTransition";

afterEach(cleanup);

function view(ready: boolean, disabled = false) {
  return <WorkspaceTransition ready={ready} entry={<button>连接</button>}>
    <textarea aria-label="输入消息" disabled={disabled} />
  </WorkspaceTransition>;
}

describe("workspace entry", () => {
  it("shows the entry until ready, then immediately shows and focuses the workspace", () => {
    const page = render(view(false));
    expect(screen.getByRole("button", { name: "连接" })).toBeVisible();
    expect(screen.queryByRole("textbox", { name: "输入消息" })).toBeNull();

    page.rerender(view(true));
    expect(screen.getByRole("textbox", { name: "输入消息" })).toHaveFocus();
    expect(screen.queryByRole("button", { name: "连接" })).toBeNull();
    expect(page.container.querySelector("canvas")).toBeNull();

    page.rerender(view(false));
    expect(screen.getByRole("button", { name: "连接" })).toBeVisible();
    expect(screen.queryByRole("textbox", { name: "输入消息" })).toBeNull();
  });

  it("focuses the workspace when the composer is disabled", () => {
    const page = render(view(true, true));
    expect(page.container.querySelector('[data-workspace-transition="complete"] > div')).toHaveFocus();
  });
});
