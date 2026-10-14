import { THEME_PRESETS, type ThemeId } from "../../styles/themePresets";
import type { editor } from "monaco-editor";

type MonacoThemeApi = Pick<typeof editor, "defineTheme" | "setTheme">;
let loaded: MonacoThemeApi | null = null;

/** 只保留已加载的 SDK；当前主题始终由应用 owner 提供，不在这里缓存选择。 */
export function registerMonacoThemes(api: MonacoThemeApi): void {
  if (loaded === api) return;
  for (const [id, preset] of Object.entries(THEME_PRESETS)) {
    const values: Readonly<Record<string, string>> = preset.colors;
    const color = (role: string): string => {
      const value = values[`--ez-${role}`];
      const variable = value?.match(/^var\(--ez-([\w-]+)\)$/)?.[1];
      return variable ? color(variable) : value;
    };
    api.defineTheme(`ez-${id.replaceAll("_", "-")}`, {
      base: preset.color_mode === "dark" ? "vs-dark" : "vs", inherit: true,
      rules: Object.entries(preset.syntax).map(([token, value]) => ({ token, foreground: value.slice(1) })),
      colors: {
        "editor.background": color("surface"), "editor.foreground": color("text"),
        "editorLineNumber.foreground": color("text-muted"), "editorLineNumber.activeForeground": color("text"),
        "editorCursor.foreground": color("primary-text"), "editor.selectionBackground": color("terminal-selection"),
        "editor.inactiveSelectionBackground": color("terminal-selection-inactive"),
        "editor.lineHighlightBackground": color("surface-subtle"), "editor.lineHighlightBorder": color("surface-subtle"),
        "editor.findMatchBackground": color("primary-soft"), "editor.findMatchBorder": color("primary-text"),
        "editor.findMatchHighlightBackground": color("terminal-selection"),
        "editorWidget.background": color("surface-subtle"), "editorWidget.foreground": color("text"), "editorWidget.border": color("border-strong"),
        "input.background": color("surface"), "input.foreground": color("text"), "input.border": color("border"),
        "input.placeholderForeground": color("text-muted"), "focusBorder": color("focus"),
        "menu.background": color("surface"), "menu.foreground": color("text"), "menu.border": color("border"),
        "menu.selectionBackground": color("primary-soft"), "menu.selectionForeground": color("primary-text"),
        "scrollbarSlider.background": color("terminal-selection-inactive"), "scrollbarSlider.hoverBackground": color("terminal-selection"),
      },
    });
  }
  loaded = api;
}

export function applyMonacoTheme(theme: ThemeId): void { loaded?.setTheme(`ez-${theme.replaceAll("_", "-")}`); }
