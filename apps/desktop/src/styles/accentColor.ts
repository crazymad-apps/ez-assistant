import {
  ACCENT_SWATCH_OPTIONS,
  normalizeHexColor,
  type AccentPresetId,
} from "./appearanceOptions";

type Rgb = Readonly<{ r: number; g: number; b: number }>;

function parseHex(hex: string): Rgb {
  const normalized = normalizeHexColor(hex) ?? "#f97316";
  return {
    r: Number.parseInt(normalized.slice(1, 3), 16),
    g: Number.parseInt(normalized.slice(3, 5), 16),
    b: Number.parseInt(normalized.slice(5, 7), 16),
  };
}

function toHex(rgb: Rgb): string {
  const clamp = (value: number) => Math.max(0, Math.min(255, Math.round(value)));
  return `#${[clamp(rgb.r), clamp(rgb.g), clamp(rgb.b)]
    .map((channel) => channel.toString(16).padStart(2, "0"))
    .join("")}`;
}

function mix(base: Rgb, target: Rgb, ratio: number): Rgb {
  return {
    r: base.r + (target.r - base.r) * ratio,
    g: base.g + (target.g - base.g) * ratio,
    b: base.b + (target.b - base.b) * ratio,
  };
}

function relativeLuminance(rgb: Rgb): number {
  const linear = [rgb.r, rgb.g, rgb.b].map((channel) => {
    const srgb = channel / 255;
    return srgb <= 0.03928 ? srgb / 12.92 : ((srgb + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}

function contrastRatio(first: Rgb, second: Rgb): number {
  const l1 = relativeLuminance(first);
  const l2 = relativeLuminance(second);
  const lighter = Math.max(l1, l2);
  const darker = Math.min(l1, l2);
  return (lighter + 0.05) / (darker + 0.05);
}

function ensureReadableText(accent: Rgb, background: Rgb, color_mode: "light" | "dark"): Rgb {
  const target = color_mode === "light" ? { r: 16, g: 20, b: 28 } : { r: 245, g: 247, b: 252 };
  if (contrastRatio(accent, background) >= 4.5) return accent;
  let adjusted = accent;
  for (let step = 1; step <= 12; step += 1) {
    adjusted = mix(accent, target, step / 12);
    if (contrastRatio(adjusted, background) >= 4.5) return adjusted;
  }
  return adjusted;
}

export function resolveAccentHex(preset_id: AccentPresetId, custom_hex: string): string | null {
  if (preset_id === "theme") return null;
  if (preset_id === "custom") return normalizeHexColor(custom_hex) ?? "#f97316";
  return ACCENT_SWATCH_OPTIONS.find((item) => item.id === preset_id)?.hex ?? "#f97316";
}

/** 依据单主色与当前明暗底色自动派生对比度合规的 CSS Token 覆盖集。 */
export function resolveAccentColors(
  preset_id: AccentPresetId,
  custom_hex: string,
  color_mode: "light" | "dark",
  surface_hex?: string,
): Readonly<Record<string, string>> | null {
  const accent_hex = resolveAccentHex(preset_id, custom_hex);
  if (!accent_hex) return null;

  const base = parseHex(accent_hex);
  const white: Rgb = { r: 255, g: 255, b: 255 };
  const dark_ink: Rgb = { r: 20, g: 21, b: 24 };
  const surface = parseHex(surface_hex ?? (color_mode === "dark" ? "#141518" : "#ffffff"));

  // 深色背景若选了过暗的主色，轻微提亮以保持按钮辨识度。
  const primary_rgb =
    color_mode === "dark" && relativeLuminance(base) < 0.08
      ? mix(base, white, 0.25)
      : base;
  const primary = toHex(primary_rgb);
  const hover_rgb =
    color_mode === "light" ? mix(primary_rgb, dark_ink, 0.12) : mix(primary_rgb, white, 0.14);
  const primary_text_rgb = ensureReadableText(primary_rgb, surface, color_mode);
  const primary_text = toHex(primary_text_rgb);
  const on_primary = contrastRatio(primary_rgb, white) >= contrastRatio(primary_rgb, dark_ink) ? "#ffffff" : "#141518";
  const primary_soft = toHex(mix(surface, primary_rgb, color_mode === "light" ? 0.11 : 0.2));
  const primary_border = toHex(mix(surface, primary_rgb, color_mode === "light" ? 0.28 : 0.42));

  return {
    "--ez-primary": primary,
    "--ez-primary-hover": toHex(hover_rgb),
    "--ez-primary-text": primary_text,
    "--ez-primary-soft": primary_soft,
    "--ez-primary-border": primary_border,
    "--ez-on-primary": on_primary,
    "--ez-focus": primary_text,
    "--ez-terminal-cursor": primary_text,
    "--ez-terminal-selection": `${primary}38`,
    "--ez-terminal-blue": primary_text,
    "--ez-terminal-bright-blue": primary,
    "--ez-entry-dots": primary_text,
  };
}
