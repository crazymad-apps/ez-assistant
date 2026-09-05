import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { InlineIconButton } from "../../src/components/InlineIconButton";

describe("InlineIconButton", () => {
  it("provides an accessible label while rendering only the requested icon", () => {
    render(<InlineIconButton icon="copy" label="复制路径" size={13} />);

    const button = screen.getByRole("button", { name: "复制路径" });
    expect(button).toHaveAttribute("type", "button");
    expect(button).toHaveTextContent("");
    expect(button.querySelector("svg")).toHaveAttribute("width", "13");
    expect(button.querySelector("svg")).toHaveAttribute("height", "13");
  });
});
