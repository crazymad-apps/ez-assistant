import presets from '../../theme-presets.json';

export type ThemeId = keyof typeof presets;

export type ThemePreset = {
  readonly name: string;
  readonly color_mode: 'light' | 'dark';
  readonly startup_background: string;
  readonly colors: Readonly<typeof presets.default_light.colors>;
  readonly code_theme: string;
  readonly syntax: Readonly<typeof presets.default_light.syntax>;
};

export const DEFAULT_THEME_ID: ThemeId = 'default_light';

/** 仅为应用内置常量；用户偏好只保存标识，原生校验也读取这同一份表。 */
export const THEME_PRESETS: Readonly<Record<ThemeId, ThemePreset>> = Object.freeze(
  Object.fromEntries(
    Object.entries(presets).map(([id, preset]) => {
      if (preset.color_mode !== 'light' && preset.color_mode !== 'dark') {
        throw new Error(`Invalid built-in theme mode: ${id}`);
      }
      Object.freeze(preset.colors);
      Object.freeze(preset.syntax);
      return [id, Object.freeze(preset)];
    }),
  ),
) as Readonly<Record<ThemeId, ThemePreset>>;

export function isThemeId(value: unknown): value is ThemeId {
  return typeof value === 'string' && Object.hasOwn(THEME_PRESETS, value);
}

/** 所有变量先写完再发布主题标识；终端观察器及 UI 只消费这个完整投影。 */
export function applyDocumentTheme(theme_id: ThemeId, native_failed = false): void {
  const preset = THEME_PRESETS[theme_id];
  const root = document.documentElement;
  for (const [name, value] of Object.entries(preset.colors)) root.style.setProperty(name, value);
  root.style.colorScheme = preset.color_mode;
  root.classList.toggle('dark', preset.color_mode === 'dark');
  root.dataset.colorMode = preset.color_mode;
  root.dataset.nativeThemeFailed = String(native_failed);
  root.dataset.theme = theme_id;
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content', preset.startup_background);
}
