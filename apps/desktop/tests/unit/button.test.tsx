import { createRef } from "react";
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Button } from "../../src/components/Button";

describe("Button", () => {
  it("applies the shared visual contract and defaults to a non-submit button", () => {
    const ref = createRef<HTMLButtonElement>();
    render(
      <Button className="caller-layout" iconOnly ref={ref} size="small" variant="text">
        添加
      </Button>,
    );

    const button = screen.getByRole("button", { name: "添加" });
    expect(button).toHaveAttribute("type", "button");
    expect(button).toHaveAttribute("data-button-icon-only", "true");
    expect(button).toHaveAttribute("data-button-variant", "text");
    expect(button).toHaveAttribute("data-size", "small");
    expect(button).toHaveClass("caller-layout");
    expect(ref.current).toBe(button);
  });

  it("preserves native button attributes", () => {
    render(<Button disabled type="submit" variant="primary">保存</Button>);

    const button = screen.getByRole("button", { name: "保存" });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute("type", "submit");
    expect(button).toHaveAttribute("data-button-variant", "primary");
  });
});
