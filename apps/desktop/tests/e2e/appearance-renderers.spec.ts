import { expect, test } from "@playwright/test";
import type { TerminalEmulator } from "../../src/features/resource-workspace/terminalEmulator";

type RendererWindow = Window & { rendererTerminal: TerminalEmulator; savedEditor: Element | null; savedTerminal: TerminalEmulator["terminal"]; savedScroll: number };

test("theme changes preserve editor, terminal history and selection while syntax and diagrams update", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto("/tests/support/renderers/index.html");
  await expect(page.locator(".monaco-editor")).toBeVisible();
  await expect(page.locator('[data-streamdown="mermaid-block"] svg')).toBeVisible();
  await page.waitForFunction(() => (window as RendererWindow).rendererTerminal?.terminal.getSelection() === "history");
  await page.evaluate(() => {
    const state = window as RendererWindow;
    state.savedEditor = document.querySelector(".monaco-editor");
    state.savedTerminal = state.rendererTerminal.terminal;
    state.savedScroll = document.querySelector(".monaco-scrollable-element")!.scrollTop;
  });
  const cases = [
    ["Monokai", "monokai", "rgb(102, 217, 239)", "rgb(39, 40, 34)"],
    ["Solarized Light", "solarized_light", "rgb(88, 110, 117)", "rgb(253, 246, 227)"],
    ["Solarized Dark", "solarized_dark", "rgb(147, 161, 161)", "rgb(0, 43, 54)"],
    ["默认浅色", "default_light", "rgb(215, 58, 73)", "rgb(255, 255, 255)"],
  ];
  for (const [name, id, keyword, background] of cases) {
    await page.getByRole("button", { name, exact: true }).click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", id);
    await expect(page.locator('[data-streamdown="mermaid-block"] svg')).toBeVisible();
    const token = page.locator('[data-streamdown="code-block-body"] span[style]').first();
    await expect(token).toHaveText("const");
    await expect(token).toHaveCSS("color", keyword);
    await expect(page.locator(".monaco-editor")).toHaveCSS("background-color", background);
    expect(await page.evaluate(() => {
      const state = window as RendererWindow;
      return { editor: state.savedEditor === document.querySelector(".monaco-editor"), terminal: state.savedTerminal === state.rendererTerminal.terminal,
        history: state.rendererTerminal.terminal.buffer.active.getLine(0)?.translateToString().trim(), selection: state.rendererTerminal.terminal.getSelection(),
        scroll: document.querySelector(".monaco-scrollable-element")!.scrollTop === state.savedScroll };
    })).toEqual({ editor: true, terminal: true, history: "history stays intact", selection: "history", scroll: true });
  }
  expect(errors).toEqual([]);
});
