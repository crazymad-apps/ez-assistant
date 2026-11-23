import { expect, test } from "@playwright/test";
import type { TerminalEmulator } from "../../src/features/resource-workspace/terminalEmulator";

test.use({ launchOptions: { ignoreDefaultArgs: ["--hide-scrollbars"] } });

type RendererWindow = Window & { rendererTerminal: TerminalEmulator; savedEditor: Element | null; savedTerminal: TerminalEmulator["terminal"]; savedScroll: number };

test("Markdown table text uses the full cell width before wrapping", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 720 });
  await page.goto("/tests/support/renderers/index.html");
  const cell = page.getByRole("cell", { name: "预发环境（已验证）、验收环境（同步完成）", exact: true });
  const lineCount = () => cell.evaluate((element) => {
    const range = document.createRange();
    range.selectNodeContents(element.firstElementChild ?? element);
    return new Set([...range.getClientRects()].map((rect) => Math.round(rect.top))).size;
  });
  await expect.poll(lineCount).toBe(1);

  await page.setViewportSize({ width: 320, height: 720 });
  await expect.poll(lineCount).toBeGreaterThan(1);
});

test("wide Markdown tables expose a draggable horizontal scrollbar", async ({ page }) => {
  await page.setViewportSize({ width: 540, height: 720 });
  await page.goto("/tests/support/renderers/index.html");
  const table = page.getByRole("table").filter({ has: page.getByRole("columnheader", { name: "最终结果" }) });
  const scroller = table.locator("..");
  await scroller.scrollIntoViewIfNeeded();
  const track = await scroller.evaluate((element) => {
    const { left, bottom } = element.getBoundingClientRect();
    const height = (element as HTMLElement).offsetHeight - element.clientHeight;
    return {
      x: left + 12,
      y: bottom - height / 2,
      height,
      drag: element.clientWidth - 24,
    };
  });
  expect(track.height).toBeGreaterThan(0);
  await page.mouse.move(track.x, track.y);
  await page.mouse.down();
  await page.mouse.move(track.x + track.drag, track.y, { steps: 8 });
  await page.mouse.up();
  await expect.poll(() => scroller.evaluate((element) => element.scrollLeft)).toBeGreaterThan(0);
  await expect(table.getByRole("cell", { name: "通过回归测试" })).toBeInViewport({ ratio: 1 });

  await page.setViewportSize({ width: 1200, height: 720 });
  await expect.poll(() => scroller.evaluate((element) => element.scrollWidth - element.clientWidth)).toBe(0);
});

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
