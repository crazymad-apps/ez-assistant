import type { ITheme, Terminal } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";

export type TerminalEmulator = Readonly<{ terminal: Terminal; fit: FitAddon; host: HTMLDivElement }>;

/** 按需加载官方 DOM renderer；CSS 与实现同批进入，终端实例由标签 owner 持有。 */
export async function createTerminalEmulator(): Promise<TerminalEmulator> {
  const [{ Terminal }, { FitAddon }] = await Promise.all([
    import("@xterm/xterm"), import("@xterm/addon-fit"), import("@xterm/xterm/css/xterm.css"),
  ]);
  const terminal = new Terminal({
    cols: 80, rows: 24, scrollback: 5000, fontSize: 13, lineHeight: 1.2,
    fontFamily: 'Menlo, Monaco, Consolas, "Liberation Mono", monospace',
    cursorBlink: true, allowProposedApi: false, minimumContrastRatio: 4.5,
    theme: readTerminalTheme(),
  });
  // xterm 不直接消费 CSS 变量；更新现有实例，保留 PTY、缓冲和选区。
  // 跟随根主题属性及系统外观变化，监听由 xterm 的 addon 生命周期统一回收。
  const updateTheme = () => { terminal.options.theme = readTerminalTheme(); };
  const themeObserver = new MutationObserver(updateTheme);
  const systemTheme = window.matchMedia("(prefers-color-scheme: dark)");
  terminal.loadAddon({
    activate: () => {
      themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "style", "data-theme"] });
      systemTheme.addEventListener("change", updateTheme);
    },
    dispose: () => {
      themeObserver.disconnect();
      systemTheme.removeEventListener("change", updateTheme);
    },
  });
  const fit = new FitAddon();
  terminal.loadAddon(fit);
  const host = document.createElement("div");
  terminal.open(host);
  return { terminal, fit, host };
}

function readTerminalTheme(): ITheme {
  const palette = getComputedStyle(document.documentElement);
  const color = (name: string) => palette.getPropertyValue(`--ez-terminal-${name}`).trim();
  return {
    background: color("background"), foreground: color("foreground"),
    cursor: color("cursor"), cursorAccent: color("background"),
    selectionBackground: color("selection"), selectionInactiveBackground: color("selection-inactive"),
    black: color("black"), red: color("red"), green: color("green"), yellow: color("yellow"),
    blue: color("blue"), magenta: color("magenta"), cyan: color("cyan"), white: color("white"),
    brightBlack: color("bright-black"), brightRed: color("bright-red"), brightGreen: color("bright-green"),
    brightYellow: color("bright-yellow"), brightBlue: color("bright-blue"), brightMagenta: color("bright-magenta"),
    brightCyan: color("bright-cyan"), brightWhite: color("bright-white"),
  };
}
