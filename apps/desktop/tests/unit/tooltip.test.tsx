import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { Tooltip } from "../../src/components/Tooltip";

afterEach(cleanup);

describe("Tooltip", () => {
  it("shows for keyboard focus and closes with Escape", async () => {
    const user = userEvent.setup();
    render(
      <Tooltip content="添加附件">
        <button aria-label="附件" type="button">+</button>
      </Tooltip>,
    );

    await user.tab();
    expect(screen.getByRole("tooltip")).toHaveTextContent("添加附件");
    expect(screen.getByRole("button", { name: "附件" })).toHaveAttribute("aria-describedby");

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  });
});
