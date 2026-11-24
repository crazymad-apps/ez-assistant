import presets from "../../theme-presets.json";

export type AppearanceMode = "system" | "light" | "dark";

export type AccentPresetId =
  | "theme"
  | "orange"
  | "blue"
  | "emerald"
  | "violet"
  | "pink"
  | "crimson"
  | "amber"
  | "cyan"
  | "slate"
  | "custom";

export type FontSizeScale = "xs" | "sm" | "md" | "lg" | "xl";
export type LineHeightPreset = "compact" | "normal" | "relaxed" | "loose";
export type UiFontId = "system" | "pingfang" | "harmony" | "lxgw" | "serif" | "rounded" | "mono" | "custom";
export type ProseFontId = "inherit" | "sans" | "serif" | "lxgw" | "mono" | "custom";
export type CodeFontId = "system" | "jetbrains_mono" | "fira_code" | "maple_mono" | "cascadia_code" | "custom";
export type ContentWidthPreset = "narrow" | "default" | "wide" | "full";
export type DensityPreset = "compact" | "default" | "spacious";

export type AppearanceThemeId = keyof typeof presets;

export type AppearancePreferences = Readonly<{
  mode: AppearanceMode;
  light_theme_id: AppearanceThemeId;
  dark_theme_id: AppearanceThemeId;
  accent_preset: AccentPresetId;
  custom_accent_hex: string;
  font_size: FontSizeScale;
  ui_font: UiFontId;
  custom_ui_font: string;
  prose_font: ProseFontId;
  custom_prose_font: string;
  line_height: LineHeightPreset;
  code_font: CodeFontId;
  custom_code_font: string;
  code_ligatures: boolean;
  content_width: ContentWidthPreset;
  density: DensityPreset;
  show_sidebar_time: boolean;
  show_message_timestamps: boolean;
  macos_vibrancy: boolean;
}>;

export const DEFAULT_APPEARANCE_PREFERENCES: AppearancePreferences = Object.freeze({
  mode: "system",
  light_theme_id: "default_light",
  dark_theme_id: "default_dark",
  accent_preset: "theme",
  custom_accent_hex: "#f97316",
  font_size: "md",
  ui_font: "system",
  custom_ui_font: "",
  prose_font: "inherit",
  custom_prose_font: "",
  line_height: "relaxed",
  code_font: "system",
  custom_code_font: "",
  code_ligatures: true,
  content_width: "default",
  density: "default",
  show_sidebar_time: true,
  show_message_timestamps: true,
  macos_vibrancy: true,
});

export type AccentSwatchOption = Readonly<{
  id: Exclude<AccentPresetId, "custom">;
  label: string;
  hex: string | null;
}>;

export const ACCENT_SWATCH_OPTIONS: ReadonlyArray<AccentSwatchOption> = Object.freeze([
  { id: "theme", label: "跟随主题默认", hex: null },
  { id: "orange", label: "活力橙", hex: "#f97316" },
  { id: "blue", label: "钴蓝", hex: "#3b82f6" },
  { id: "emerald", label: "翡翠绿", hex: "#10b981" },
  { id: "violet", label: "丁香紫", hex: "#8b5cf6" },
  { id: "pink", label: "玫瑰粉", hex: "#ec4899" },
  { id: "crimson", label: "珊瑚红", hex: "#ef4444" },
  { id: "amber", label: "琥珀金", hex: "#f59e0b" },
  { id: "cyan", label: "青碧", hex: "#06b6d4" },
  { id: "slate", label: "石板灰", hex: "#64748b" },
]);

export const FONT_SIZE_OPTIONS: ReadonlyArray<Readonly<{
  value: FontSizeScale;
  label: string;
  description: string;
  small_px: number;
  base_px: number;
  large_px: number;
}>> = Object.freeze([
  { value: "xs", label: "极小 (12px)", description: "高信息密度视图", small_px: 11, base_px: 12, large_px: 14 },
  { value: "sm", label: "较小 (13px)", description: "紧凑清晰的办公字号", small_px: 11.5, base_px: 13, large_px: 15 },
  { value: "md", label: "默认 (14px)", description: "推荐标准字号", small_px: 12, base_px: 14, large_px: 16 },
  { value: "lg", label: "较大 (15px)", description: "更轻松的长时间阅读", small_px: 13, base_px: 15, large_px: 17 },
  { value: "xl", label: "超大 (16px)", description: "高分屏大字号体验", small_px: 14, base_px: 16, large_px: 18 },
]);

export const LINE_HEIGHT_OPTIONS: ReadonlyArray<Readonly<{
  value: LineHeightPreset;
  label: string;
  description: string;
  css_value: string;
}>> = Object.freeze([
  { value: "compact", label: "紧凑 (1.45)", description: "同屏展示更多对话内容", css_value: "1.45" },
  { value: "normal", label: "标准 (1.62)", description: "均衡清晰的段落节奏", css_value: "1.62" },
  { value: "relaxed", label: "宽松 (1.78)", description: "推荐中文与混排阅读行高", css_value: "1.78" },
  { value: "loose", label: "超宽 (1.95)", description: "舒展的长文精读行距", css_value: "1.95" },
]);

const SYSTEM_UI_STACK = 'Inter, ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif';
const SYSTEM_MONO_STACK = '"SFMono-Regular", Menlo, Consolas, "Liberation Mono", monospace';
const SERIF_STACK = '"Songti SC", "Source Han Serif SC", "Noto Serif SC", Georgia, "Times New Roman", serif';
const LXGW_STACK = '"LXGW WenKai", "Kaiti SC", "STKaiti", KaiTi, sans-serif';

export const UI_FONT_OPTIONS: ReadonlyArray<Readonly<{
  value: UiFontId;
  label: string;
  description: string;
  stack: string;
}>> = Object.freeze([
  { value: "system", label: "默认（系统无衬线）", description: "随系统自动选择 Inter / 苹方 / 微软雅黑", stack: SYSTEM_UI_STACK },
  { value: "pingfang", label: "苹方 · SF Pro", description: "macOS / iOS 原生现代几何黑体", stack: '"SF Pro Text", "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", sans-serif' },
  { value: "harmony", label: "思源 · 鸿蒙黑体", description: "HarmonyOS Sans / Source Han Sans SC", stack: '"HarmonyOS Sans SC", "Source Han Sans SC", "Noto Sans CJK SC", "Microsoft YaHei", sans-serif' },
  { value: "lxgw", label: "霞鹜文楷 / 楷体", description: "温和人文手写楷体质感", stack: LXGW_STACK },
  { value: "serif", label: "经典衬线宋体", description: "端庄报刊印刷风", stack: SERIF_STACK },
  { value: "rounded", label: "系统圆体", description: "柔和亲近圆角笔画", stack: '"SF Pro Rounded", "PingFang SC", "YouYuan", ui-rounded, sans-serif' },
  { value: "mono", label: "等宽极客风", description: "全界面等宽工程美学", stack: SYSTEM_MONO_STACK },
  { value: "custom", label: "自定义本机字体…", description: "填写已安装的系统字体名称", stack: SYSTEM_UI_STACK },
]);

export const PROSE_FONT_OPTIONS: ReadonlyArray<Readonly<{
  value: ProseFontId;
  label: string;
  description: string;
  stack: string | null;
}>> = Object.freeze([
  { value: "inherit", label: "默认（界面字体）", description: "与当前界面字体保持一致", stack: null },
  { value: "sans", label: "现代无衬线", description: "清晰利落的技术与对话排版", stack: SYSTEM_UI_STACK },
  { value: "serif", label: "书卷衬线（宋体 / Georgia）", description: "适合深度阅读与长篇文档", stack: SERIF_STACK },
  { value: "lxgw", label: "霞鹜文楷 · 楷体", description: "柔和温润、接近纸质手写笔记", stack: LXGW_STACK },
  { value: "mono", label: "等宽终端风", description: "代码文档与结构化文本对齐", stack: SYSTEM_MONO_STACK },
  { value: "custom", label: "自定义本机字体…", description: "填写阅读正文的本机字体名称", stack: null },
]);

export const CODE_FONT_OPTIONS: ReadonlyArray<Readonly<{
  value: CodeFontId;
  label: string;
  description: string;
  stack: string;
}>> = Object.freeze([
  { value: "system", label: "默认（系统等宽）", description: "SF Mono / Menlo / Consolas", stack: SYSTEM_MONO_STACK },
  { value: "jetbrains_mono", label: "JetBrains Mono", description: "高可读性开发者字体", stack: `"JetBrains Mono", ${SYSTEM_MONO_STACK}` },
  { value: "fira_code", label: "Fira Code", description: "经典编程连字字体", stack: `"Fira Code", ${SYSTEM_MONO_STACK}` },
  { value: "maple_mono", label: "Maple Mono", description: "中英文 2:1 等宽斜体圆角字体", stack: `"Maple Mono NF CN", "Maple Mono", ${SYSTEM_MONO_STACK}` },
  { value: "cascadia_code", label: "Cascadia Code", description: "微软终端与 VS Code 现代等宽", stack: `"Cascadia Code", ${SYSTEM_MONO_STACK}` },
  { value: "custom", label: "自定义本机字体…", description: "填写已安装的等宽字体名称", stack: SYSTEM_MONO_STACK },
]);

export const CONTENT_WIDTH_OPTIONS: ReadonlyArray<Readonly<{
  value: ContentWidthPreset;
  label: string;
  css_value: string;
}>> = Object.freeze([
  { value: "narrow", label: "窄版", css_value: "720px" },
  { value: "default", label: "默认", css_value: "820px" },
  { value: "wide", label: "宽屏", css_value: "1040px" },
  { value: "full", label: "铺满", css_value: "100%" },
]);

export const DENSITY_OPTIONS: ReadonlyArray<Readonly<{
  value: DensityPreset;
  label: string;
}>> = Object.freeze([
  { value: "compact", label: "紧凑" },
  { value: "default", label: "默认" },
  { value: "spacious", label: "舒适" },
]);

export function sanitizeCustomFontName(raw: string): string {
  return raw
    .replace(/[\r\n\t"'\\:;{}<>]/g, "")
    .trim()
    .slice(0, 80);
}

export function normalizeHexColor(raw: string): string | null {
  const trimmed = raw.trim().toLowerCase();
  const short_match = trimmed.match(/^#([0-9a-f]{3})$/);
  if (short_match) {
    const [r, g, b] = short_match[1];
    return `#${r}${r}${g}${g}${b}${b}`;
  }
  if (/^#[0-9a-f]{6}$/.test(trimmed)) return trimmed;
  return null;
}

export function resolveUiFontStack(preferences: AppearancePreferences): string {
  if (preferences.ui_font === "custom") {
    const custom = sanitizeCustomFontName(preferences.custom_ui_font);
    if (custom) return `"${custom}", ${SYSTEM_UI_STACK}`;
  }
  return UI_FONT_OPTIONS.find((item) => item.value === preferences.ui_font)?.stack ?? SYSTEM_UI_STACK;
}

export function resolveProseFontStack(preferences: AppearancePreferences): string {
  if (preferences.prose_font === "custom") {
    const custom = sanitizeCustomFontName(preferences.custom_prose_font);
    if (custom) return `"${custom}", ${resolveUiFontStack(preferences)}`;
  }
  const matched = PROSE_FONT_OPTIONS.find((item) => item.value === preferences.prose_font)?.stack;
  return matched ?? "var(--ez-font-ui)";
}

export function resolveCodeFontStack(preferences: AppearancePreferences): string {
  if (preferences.code_font === "custom") {
    const custom = sanitizeCustomFontName(preferences.custom_code_font);
    if (custom) return `"${custom}", ${SYSTEM_MONO_STACK}`;
  }
  return CODE_FONT_OPTIONS.find((item) => item.value === preferences.code_font)?.stack ?? SYSTEM_MONO_STACK;
}

function isKnownThemeMode(value: unknown, mode: "light" | "dark"): value is AppearanceThemeId {
  if (typeof value !== "string" || !Object.hasOwn(presets, value)) return false;
  return presets[value as AppearanceThemeId].color_mode === mode;
}

function oneOf<T extends string>(value: unknown, valid: readonly T[], fallback: T): T {
  return typeof value === "string" && (valid as readonly string[]).includes(value) ? (value as T) : fallback;
}

export function defaultsFromTheme(theme_id: AppearanceThemeId): AppearancePreferences {
  const preset = presets[theme_id];
  const color_mode: "light" | "dark" = preset.color_mode === "dark" ? "dark" : "light";
  return {
    ...DEFAULT_APPEARANCE_PREFERENCES,
    mode: color_mode,
    light_theme_id: color_mode === "light" ? theme_id : "default_light",
    dark_theme_id: color_mode === "dark" ? theme_id : "default_dark",
  };
}

export function normalizeAppearancePreferences(
  input: unknown,
  fallback_theme_id: AppearanceThemeId,
): AppearancePreferences {
  const base = defaultsFromTheme(fallback_theme_id);
  if (!input || typeof input !== "object" || Array.isArray(input)) {
    return base;
  }
  const record = input as Record<string, unknown>;
  const mode = oneOf(record.mode, ["system", "light", "dark"] as const, base.mode);
  const light_theme_id = isKnownThemeMode(record.light_theme_id, "light")
    ? record.light_theme_id
    : base.light_theme_id;
  const dark_theme_id = isKnownThemeMode(record.dark_theme_id, "dark")
    ? record.dark_theme_id
    : base.dark_theme_id;
  const accent_preset = oneOf(
    record.accent_preset,
    ["theme", "orange", "blue", "emerald", "violet", "pink", "crimson", "amber", "cyan", "slate", "custom"] as const,
    DEFAULT_APPEARANCE_PREFERENCES.accent_preset,
  );
  const custom_accent_hex =
    (typeof record.custom_accent_hex === "string" && normalizeHexColor(record.custom_accent_hex)) ||
    DEFAULT_APPEARANCE_PREFERENCES.custom_accent_hex;

  return {
    mode,
    light_theme_id,
    dark_theme_id,
    accent_preset,
    custom_accent_hex,
    font_size: oneOf(record.font_size, ["xs", "sm", "md", "lg", "xl"] as const, DEFAULT_APPEARANCE_PREFERENCES.font_size),
    ui_font: oneOf(record.ui_font, ["system", "pingfang", "harmony", "lxgw", "serif", "rounded", "mono", "custom"] as const, DEFAULT_APPEARANCE_PREFERENCES.ui_font),
    custom_ui_font: typeof record.custom_ui_font === "string" ? sanitizeCustomFontName(record.custom_ui_font) : "",
    prose_font: oneOf(record.prose_font, ["inherit", "sans", "serif", "lxgw", "mono", "custom"] as const, DEFAULT_APPEARANCE_PREFERENCES.prose_font),
    custom_prose_font: typeof record.custom_prose_font === "string" ? sanitizeCustomFontName(record.custom_prose_font) : "",
    line_height: oneOf(record.line_height, ["compact", "normal", "relaxed", "loose"] as const, DEFAULT_APPEARANCE_PREFERENCES.line_height),
    code_font: oneOf(record.code_font, ["system", "jetbrains_mono", "fira_code", "maple_mono", "cascadia_code", "custom"] as const, DEFAULT_APPEARANCE_PREFERENCES.code_font),
    custom_code_font: typeof record.custom_code_font === "string" ? sanitizeCustomFontName(record.custom_code_font) : "",
    code_ligatures: typeof record.code_ligatures === "boolean" ? record.code_ligatures : DEFAULT_APPEARANCE_PREFERENCES.code_ligatures,
    content_width: oneOf(record.content_width, ["narrow", "default", "wide", "full"] as const, DEFAULT_APPEARANCE_PREFERENCES.content_width),
    density: oneOf(record.density, ["compact", "default", "spacious"] as const, DEFAULT_APPEARANCE_PREFERENCES.density),
    show_sidebar_time: typeof record.show_sidebar_time === "boolean" ? record.show_sidebar_time : DEFAULT_APPEARANCE_PREFERENCES.show_sidebar_time,
    show_message_timestamps: typeof record.show_message_timestamps === "boolean" ? record.show_message_timestamps : DEFAULT_APPEARANCE_PREFERENCES.show_message_timestamps,
    macos_vibrancy: typeof record.macos_vibrancy === "boolean" ? record.macos_vibrancy : DEFAULT_APPEARANCE_PREFERENCES.macos_vibrancy,
  };
}
