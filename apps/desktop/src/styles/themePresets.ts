import presets from "../../theme-presets.json";
import { resolveAccentColors } from "./accentColor";
import {
  CONTENT_WIDTH_OPTIONS,
  DEFAULT_APPEARANCE_PREFERENCES,
  FONT_SIZE_OPTIONS,
  LINE_HEIGHT_OPTIONS,
  resolveCodeFontStack,
  resolveProseFontStack,
  resolveUiFontStack,
  type AppearancePreferences,
} from "./appearanceOptions";

export type ThemeId = keyof typeof presets;

export type ThemePreset = {
  readonly name: string;
  readonly color_mode: "light" | "dark";
  readonly startup_background: string;
  readonly colors: Readonly<typeof presets.default_light.colors>;
  readonly code_theme: string;
  readonly syntax: Readonly<typeof presets.default_light.syntax>;
};

export const DEFAULT_THEME_ID: ThemeId = "default_light";
export const DEFAULT_DARK_THEME_ID: ThemeId = "default_dark";

/** 仅为应用内置常量；用户偏好只保存标识，原生校验也读取这同一份表。 */
export const THEME_PRESETS: Readonly<Record<ThemeId, ThemePreset>> = Object.freeze(
  Object.fromEntries(
    Object.entries(presets).map(([id, preset]) => {
      if (preset.color_mode !== "light" && preset.color_mode !== "dark") {
        throw new Error(`Invalid built-in theme mode: ${id}`);
      }
      Object.freeze(preset.colors);
      Object.freeze(preset.syntax);
      return [id, Object.freeze(preset)];
    }),
  ),
) as Readonly<Record<ThemeId, ThemePreset>>;

export const LIGHT_THEME_IDS: ReadonlyArray<ThemeId> = Object.freeze(
  (Object.keys(THEME_PRESETS) as ThemeId[]).filter((id) => THEME_PRESETS[id].color_mode === "light"),
);

export const DARK_THEME_IDS: ReadonlyArray<ThemeId> = Object.freeze(
  (Object.keys(THEME_PRESETS) as ThemeId[]).filter((id) => THEME_PRESETS[id].color_mode === "dark"),
);

export function isThemeId(value: unknown): value is ThemeId {
  return typeof value === "string" && Object.hasOwn(THEME_PRESETS, value);
}

/** 所有变量先写完再发布主题标识；终端观察器及 UI 只消费这个完整投影。 */
export function applyDocumentTheme(
  theme_id: ThemeId,
  native_failed = false,
  preferences: AppearancePreferences = DEFAULT_APPEARANCE_PREFERENCES,
): void {
  const preset = THEME_PRESETS[theme_id];
  const root = document.documentElement;
  for (const [name, value] of Object.entries(preset.colors)) {
    root.style.setProperty(name, value);
  }

  const accent_overrides = resolveAccentColors(
    preferences.accent_preset,
    preferences.custom_accent_hex,
    preset.color_mode,
    preset.colors["--ez-surface"],
  );
  if (accent_overrides) {
    for (const [name, value] of Object.entries(accent_overrides)) {
      root.style.setProperty(name, value);
    }
  }

  const font_scale =
    FONT_SIZE_OPTIONS.find((item) => item.value === preferences.font_size) ?? FONT_SIZE_OPTIONS[2];
  const line_height =
    LINE_HEIGHT_OPTIONS.find((item) => item.value === preferences.line_height) ?? LINE_HEIGHT_OPTIONS[2];
  const content_width =
    CONTENT_WIDTH_OPTIONS.find((item) => item.value === preferences.content_width) ?? CONTENT_WIDTH_OPTIONS[1];

  root.style.setProperty("--ez-font-small", `${font_scale.small_px}px`);
  root.style.setProperty("--ez-font-base", `${font_scale.base_px}px`);
  root.style.setProperty("--ez-font-large", `${font_scale.large_px}px`);
  root.style.setProperty("--ez-font-ui", resolveUiFontStack(preferences));
  root.style.setProperty("--ez-font-prose", resolveProseFontStack(preferences));
  root.style.setProperty("--ez-font-mono", resolveCodeFontStack(preferences));
  root.style.setProperty("--ez-prose-line-height", line_height.css_value);
  root.style.setProperty("--ez-code-ligatures", preferences.code_ligatures ? "contextual" : "none");
  root.style.setProperty("--conversation-content-max-width", content_width.css_value);

  root.style.colorScheme = preset.color_mode;
  root.classList.toggle("dark", preset.color_mode === "dark");
  root.dataset.colorMode = preset.color_mode;
  root.dataset.nativeThemeFailed = String(native_failed);
  root.dataset.appearanceMode = preferences.mode;
  root.dataset.accent = preferences.accent_preset;
  root.dataset.density = preferences.density;
  root.dataset.codeLigatures = String(preferences.code_ligatures);
  root.dataset.showSidebarTime = String(preferences.show_sidebar_time);
  root.dataset.showTimestamps = String(preferences.show_message_timestamps);
  root.dataset.macosVibrancy = String(preferences.macos_vibrancy);
  root.dataset.theme = theme_id;
  document
    .querySelector('meta[name="theme-color"]')
    ?.setAttribute("content", preset.startup_background);
}
