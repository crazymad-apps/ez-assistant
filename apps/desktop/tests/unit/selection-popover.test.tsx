import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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
  it.each(["small", "default", "large"] as const)("uses the same %s size contract for button and editable triggers", (size) => {
    const shared = { size, open: false, on_open_change: vi.fn(), on_select: vi.fn(), selected: "keep", options: [{ value: "keep", label: "保持原值" }] };
    render(<><SelectionPopover {...shared} aria_label="尺寸选择" trigger_variant="field" />
      <SelectionPopover {...shared} aria_label="可编辑尺寸选择" editable trigger_variant="field" /></>);
    expect(screen.getByRole("button", { name: "尺寸选择" })).toHaveAttribute("data-size", size);
    const frame = screen.getByRole("combobox", { name: "可编辑尺寸选择" }).parentElement;
    expect(frame).toHaveAttribute("data-size", size);
    expect(frame).toHaveAttribute("data-control", "field");
  });

  it("defaults the trigger size without coupling it to its visual variant", () => {
    render(<ExampleSelection />);
    expect(screen.getByRole("button", { name: "选择示例" })).toHaveAttribute("data-size", "default");
  });

  it("derives option layout from its content without page-specific sizing props", () => {
    render(<SelectionPopover aria_label="选项内容布局" open on_open_change={() => {}} on_select={() => {}}
      selected="plain" options={[
        { value: "plain", label: "保持原值" },
        { value: "description", label: "带说明", description: "辅助说明" },
        { value: "icon", label: "带图标", icon: <span aria-hidden="true">★</span> },
      ]} />);
    const plain = screen.getByRole("option", { name: "保持原值" });
    expect(plain).toHaveAttribute("data-has-description", "false");
    expect(plain).toHaveAttribute("data-has-icon", "false");
    expect(plain).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("option", { name: /带说明\s*辅助说明/ })).toHaveAttribute("data-has-description", "true");
    expect(screen.getByRole("option", { name: "带图标" })).toHaveAttribute("data-has-icon", "true");
  });

  it("keeps native selects out of business views so they reuse the shared selection UI", () => {
    const view_sources = import.meta.glob<string>(["../../src/**/*.tsx", "!../../src/components/**"], {
      query: "?raw", import: "default", eager: true,
    });
    const native_select_views = Object.entries(view_sources)
      .filter(([, source]) => /<select(?:\s|\/?>)/.test(source))
      .map(([path]) => path);
    expect(Object.keys(view_sources).length).toBeGreaterThan(0);
    expect(native_select_views, "业务选择框必须复用 SelectionPopover，不能直接渲染原生 select").toEqual([]);
  });

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

  it("keeps the editable options open when Enter only confirms input method composition", async () => {
    const user = userEvent.setup();
    render(<EditableSelection />);
    const input = screen.getByRole("combobox", { name: "输入或选择示例" });

    await user.click(input);
    fireEvent.compositionStart(input);
    fireEvent.change(input, { target: { value: "deep" } });
    fireEvent.compositionEnd(input);
    fireEvent.keyDown(input, { key: "Enter", keyCode: 13 });

    expect(screen.getByRole("listbox")).toBeVisible();
    expect(input).toHaveValue("deep");

    fireEvent.keyUp(input, { key: "Enter", keyCode: 13 });
    fireEvent.keyDown(input, { key: "Enter", keyCode: 13 });
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
