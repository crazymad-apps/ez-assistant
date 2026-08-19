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

function EditableSelection() {
  const [open, setOpen] = useState(false);
  const [selected, setSelected] = useState("");
  return (
    <SelectionPopover
      aria_label="输入或选择示例"
      content_width="content"
      editable
      on_open_change={setOpen}
      on_select={setSelected}
      open={open}
      options={[
        { value: "deepseek", label: "DeepSeek" },
        { value: "dashscope", label: "阿里云百炼" },
      ]}
      selected={selected}
      trigger_variant="field"
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

  it("supports free input, filtering, and a single suggested value", async () => {
    const user = userEvent.setup();
    render(<EditableSelection />);
    const input = screen.getByRole("combobox", { name: "输入或选择示例" });

    await user.click(input);
    expect(screen.getAllByRole("option")).toHaveLength(2);
    await user.type(input, "deep");
    expect(screen.getAllByRole("option")).toHaveLength(1);
    await user.click(screen.getByRole("option", { name: "DeepSeek" }));

    expect(input).toHaveValue("deepseek");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("keeps a field popover at least as wide as its controlled input", async () => {
    const user = userEvent.setup();
    render(<EditableSelection />);
    const input = screen.getByRole("combobox", { name: "输入或选择示例" });
    const trigger = input.parentElement;
    expect(trigger).not.toBeNull();
    vi.spyOn(trigger as HTMLElement, "getBoundingClientRect").mockReturnValue({
      bottom: 136,
      height: 36,
      left: 40,
      right: 360,
      top: 100,
      width: 320,
      x: 40,
      y: 100,
      toJSON: () => ({}),
    });

    await user.click(input);
    await waitFor(() => expect(screen.getByRole("listbox")).toHaveStyle({ minWidth: "320px" }));
  });
});
