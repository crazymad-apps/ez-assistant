import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SelectionPopover } from "../../src/components/SelectionPopover";

afterEach(cleanup);

Element.prototype.scrollIntoView = vi.fn();

function ExampleSelection() {
  const [open, setOpen] = useState(false);
  const [selected, setSelected] = useState("one");
  return (
    <SelectionPopover
      aria_label="选择示例"
      on_open_change={setOpen}
      on_select={setSelected}
      open={open}
      options={[
        { value: "one", label: "第一项" },
        { value: "two", label: "第二项" },
        { value: "three", label: "第三项" },
      ]}
      selected={selected}
    />
  );
}

describe("SelectionPopover", () => {
  it("opens from the keyboard and moves through options", async () => {
    const user = userEvent.setup();
    render(<ExampleSelection />);
    const trigger = screen.getByRole("button", { name: "选择示例" });
    trigger.focus();

    await user.keyboard("{ArrowDown}");
    await waitFor(() => expect(screen.getByRole("option", { name: "第一项" })).toHaveFocus());

    await user.keyboard("{ArrowDown}{End}");
    expect(screen.getByRole("option", { name: "第三项" })).toHaveFocus();
  });

  it("restores focus after selection", async () => {
    const user = userEvent.setup();
    render(<ExampleSelection />);
    const trigger = screen.getByRole("button", { name: "选择示例" });

    await user.click(trigger);
    await user.click(screen.getByRole("option", { name: "第二项" }));

    expect(trigger).toHaveFocus();
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });
});
