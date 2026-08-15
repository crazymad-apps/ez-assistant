import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { useState } from "react";
import { Dialog } from "../../src/components/Dialog";

afterEach(cleanup);

function DialogHarness() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button onClick={() => setOpen(true)} type="button">打开弹窗</button>
      {open && (
        <Dialog
          aria_label="测试弹窗"
          backdrop_class_name="backdrop"
          dialog_class_name="dialog"
          on_close={() => setOpen(false)}
        >
          <button type="button">第一个操作</button>
          <button type="button">最后一个操作</button>
        </Dialog>
      )}
    </>
  );
}

describe("Dialog", () => {
  it("traps focus and restores it after Escape closes the dialog", async () => {
    const user = userEvent.setup();
    render(<DialogHarness />);
    const opener = screen.getByRole("button", { name: "打开弹窗" });

    await user.click(opener);
    const first = screen.getByRole("button", { name: "第一个操作" });
    const last = screen.getByRole("button", { name: "最后一个操作" });
    await waitFor(() => expect(first).toHaveFocus());

    await user.tab({ shift: true });
    expect(last).toHaveFocus();
    await user.tab();
    expect(first).toHaveFocus();

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "测试弹窗" })).not.toBeInTheDocument();
    expect(opener).toHaveFocus();
  });
});
