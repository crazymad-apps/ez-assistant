import { action, computed, makeObservable, observable, runInAction } from "mobx";
import {
  applyDesktopTheme,
  loadDesktopAppearance,
  loadDesktopTheme,
  saveDesktopAppearance,
  saveDesktopTheme,
  type DesktopThemePreference,
} from "../native-bridge/desktopPreferences";
import {
  DEFAULT_APPEARANCE_PREFERENCES,
  defaultsFromTheme,
  normalizeAppearancePreferences,
  type AppearanceMode,
  type AppearancePreferences,
} from "../styles/appearanceOptions";
import {
  applyDocumentTheme,
  DEFAULT_THEME_ID,
  isThemeId,
  THEME_PRESETS,
  type ThemeId,
} from "../styles/themePresets";

type AppearancePersistence = {
  readonly load: () => DesktopThemePreference | Promise<DesktopThemePreference>;
  readonly save: (theme_id: ThemeId) => void | Promise<void>;
  readonly loadAppearance?: () => unknown | Promise<unknown>;
  readonly saveAppearance?: (appearance: AppearancePreferences) => void | Promise<void>;
};

type AppearancePresentation = {
  readonly apply: (
    theme_id: ThemeId,
    native_failed: boolean,
    preferences?: AppearancePreferences,
  ) => void;
  readonly applyNative: (theme_id: ThemeId) => Promise<void>;
};

type ThemeSelection = {
  readonly theme_id: ThemeId;
  readonly appearance: AppearancePreferences | null;
};

function readSystemColorMode(): "light" | "dark" {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return "light";
  try {
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  } catch {
    return "light";
  }
}

function resolveThemeFromPreferences(
  preferences: AppearancePreferences,
  system_mode: "light" | "dark",
): ThemeId {
  const effective_mode = preferences.mode === "system" ? system_mode : preferences.mode;
  return effective_mode === "dark" ? preferences.dark_theme_id : preferences.light_theme_id;
}

/** 应用级展示偏好，不依附可替换的 RootStore；DOM 与原生窗口接入由应用装配负责。 */
export class AppearanceStore {
  theme_id: ThemeId = DEFAULT_THEME_ID;
  saved_theme_id: ThemeId | null = null;
  preferences: AppearancePreferences = DEFAULT_APPEARANCE_PREFERENCES;
  saved_preferences: AppearancePreferences | null = null;
  system_color_mode: "light" | "dark" = readSystemColorMode();

  initialized = false;
  loading = false;
  saving = false;
  recovered_unknown = false;
  read_error: string | null = null;
  save_error: string | null = null;

  native_error: string | null = null;
  applying_native = false;
  private native_selection: { readonly theme_id: ThemeId } | null = null;
  private pending_native: { readonly theme_id: ThemeId } | null = null;

  private disposed = false;
  private selection: ThemeSelection | null = null;
  private pending_save: ThemeSelection | null = null;
  private save_task: Promise<void> | null = null;
  private initializing: Promise<void> | null = null;
  private reading: { cancel: () => void } | null = null;
  private dispose_media_listener: (() => void) | null = null;

  constructor(
    private readonly persistence: AppearancePersistence = {
      load: loadDesktopTheme,
      save: saveDesktopTheme,
      loadAppearance: loadDesktopAppearance,
      saveAppearance: saveDesktopAppearance,
    },
    private readonly presentation: AppearancePresentation = {
      apply: applyDocumentTheme,
      applyNative: applyDesktopTheme,
    },
  ) {
    makeObservable(this, {
      theme_id: observable,
      saved_theme_id: observable,
      preferences: observable,
      saved_preferences: observable,
      system_color_mode: observable,
      initialized: observable,
      loading: observable,
      saving: observable,
      recovered_unknown: observable,
      read_error: observable,
      save_error: observable,
      native_error: observable,
      applying_native: observable,
      retryNativeAppearance: action,
      preset: computed,
      unsaved: computed,
      initialize: action,
      selectTheme: action,
      setMode: action,
      updatePreferences: action,
      resetAppearance: action,
      retrySave: action,
      dispose: action,
    });
    this.subscribeSystemColorMode();
  }

  get preset() {
    return THEME_PRESETS[this.theme_id];
  }

  get unsaved(): boolean {
    return (
      this.theme_id !== this.saved_theme_id ||
      this.save_error !== null ||
      (Boolean(this.persistence.saveAppearance) &&
        this.saved_preferences !== null &&
        this.preferences !== this.saved_preferences)
    );
  }

  private subscribeSystemColorMode(): void {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    try {
      const media = window.matchMedia("(prefers-color-scheme: dark)");
      const listener = (event: MediaQueryListEvent) => {
        if (this.disposed) return;
        runInAction(() => {
          this.system_color_mode = event.matches ? "dark" : "light";
          if (this.preferences.mode === "system" && this.initialized) {
            const next_theme = resolveThemeFromPreferences(this.preferences, this.system_color_mode);
            if (next_theme !== this.theme_id) {
              this.theme_id = next_theme;
              this.applyAppearance();
            }
          }
        });
      };
      if (typeof media.addEventListener === "function") {
        media.addEventListener("change", listener);
        this.dispose_media_listener = () => media.removeEventListener("change", listener);
      }
    } catch {
      // 测试环境未实现 matchMedia 时忽略系统主题监听。
    }
  }

  initialize(): Promise<void> {
    if (this.disposed || this.initialized) return Promise.resolve();
    if (this.initializing) return this.initializing;
    this.loading = true;

    this.initializing = new Promise<void>((resolve) => {
      let timer: ReturnType<typeof setTimeout>;
      const reading = { cancel: () => finish() };
      const finish = (
        value?: DesktopThemePreference,
        error?: string,
        raw_appearance?: unknown,
      ) => {
        if (this.reading !== reading) return;
        this.reading = null;
        clearTimeout(timer);
        if (!this.disposed) {
          runInAction(() => {
            this.loading = false;
            this.initialized = true;
            this.read_error = error ?? null;
            if (value) {
              const has_appearance =
                raw_appearance !== null &&
                raw_appearance !== undefined &&
                typeof raw_appearance === "object" &&
                !Array.isArray(raw_appearance);
              const next_preferences = has_appearance
                ? normalizeAppearancePreferences(raw_appearance, value.theme_id)
                : value.theme_id === DEFAULT_THEME_ID
                  ? DEFAULT_APPEARANCE_PREFERENCES
                  : defaultsFromTheme(value.theme_id);
              const next_theme = has_appearance
                ? resolveThemeFromPreferences(next_preferences, this.system_color_mode)
                : value.theme_id;
              this.preferences = next_preferences;
              this.saved_preferences = next_preferences;
              this.theme_id = next_theme;
              this.saved_theme_id = value.theme_id;
              this.recovered_unknown = value.recovered_unknown;
            }
            this.applyAppearance();
          });
        }
        resolve();
      };
      this.reading = reading;
      // 只结束这次本地等待；原生读取不可取消，迟到结果凭任务身份失效，不写默认值回磁盘。
      timer = setTimeout(() => finish(undefined, "读取配色超时，已使用默认配色。"), 1_500);
      try {
        const loaded_theme = this.persistence.load();
        const loaded_appearance = this.persistence.loadAppearance
          ? this.persistence.loadAppearance()
          : null;
        if ("then" in loaded_theme || (loaded_appearance && typeof loaded_appearance === "object" && "then" in loaded_appearance)) {
          void Promise.all([
            Promise.resolve(loaded_theme),
            Promise.resolve(loaded_appearance).catch(() => null),
          ]).then(
            ([theme_value, appearance_value]) => finish(theme_value, undefined, appearance_value),
            () => finish(undefined, "无法读取配色，已使用默认配色。"),
          );
        } else {
          // localStorage 同步完成，调用方可以在挂载首屏前直接取得主题。
          finish(loaded_theme, undefined, loaded_appearance);
        }
      } catch {
        finish(undefined, "无法读取配色，已使用默认配色。");
      }
    });
    return this.initializing;
  }

  selectTheme(theme_id: ThemeId): void {
    if (this.disposed || !isThemeId(theme_id)) return;
    if (
      theme_id === this.theme_id &&
      !this.recovered_unknown &&
      this.save_error === null &&
      (this.saving || this.saved_theme_id === theme_id)
    ) {
      return;
    }

    const chosen_mode = THEME_PRESETS[theme_id].color_mode;
    const current_effective =
      this.preferences.mode === "system" ? this.system_color_mode : this.preferences.mode;
    const next_mode: AppearanceMode =
      this.preferences.mode === "system" && chosen_mode === current_effective
        ? "system"
        : chosen_mode;
    const next_preferences: AppearancePreferences = {
      ...this.preferences,
      mode: next_mode,
      light_theme_id: chosen_mode === "light" ? theme_id : this.preferences.light_theme_id,
      dark_theme_id: chosen_mode === "dark" ? theme_id : this.preferences.dark_theme_id,
    };

    // 明确选择优先于尚未完成的启动读取，也让等待初始化的调用方立即继续。
    this.reading?.cancel();
    this.initialized = true;
    this.theme_id = theme_id;
    this.preferences = next_preferences;
    this.read_error = null;
    this.recovered_unknown = false;
    this.save_error = null;
    this.selection = {
      theme_id,
      appearance: this.persistence.saveAppearance ? next_preferences : null,
    };
    this.applyAppearance();
    this.pending_save = this.selection;
    this.startSave();
  }

  setMode(mode: AppearanceMode): void {
    if (this.disposed) return;
    const next_preferences: AppearancePreferences = { ...this.preferences, mode };
    const next_theme = resolveThemeFromPreferences(next_preferences, this.system_color_mode);
    if (
      this.preferences.mode === mode &&
      this.theme_id === next_theme &&
      this.save_error === null
    ) {
      return;
    }
    this.reading?.cancel();
    this.initialized = true;
    this.preferences = next_preferences;
    this.theme_id = next_theme;
    this.read_error = null;
    this.recovered_unknown = false;
    this.save_error = null;
    this.selection = {
      theme_id: next_theme,
      appearance: next_preferences,
    };
    this.applyAppearance();
    this.pending_save = this.selection;
    this.startSave();
  }

  updatePreferences(patch: Partial<AppearancePreferences>): void {
    if (this.disposed) return;
    const merged = normalizeAppearancePreferences(
      { ...this.preferences, ...patch },
      this.theme_id,
    );
    const next_theme = resolveThemeFromPreferences(merged, this.system_color_mode);
    this.reading?.cancel();
    this.initialized = true;
    this.preferences = merged;
    this.theme_id = next_theme;
    this.read_error = null;
    this.recovered_unknown = false;
    this.save_error = null;
    this.selection = {
      theme_id: next_theme,
      appearance: merged,
    };
    this.applyAppearance();
    this.pending_save = this.selection;
    this.startSave();
  }

  resetAppearance(): void {
    if (this.disposed) return;
    const next_preferences = DEFAULT_APPEARANCE_PREFERENCES;
    const next_theme = resolveThemeFromPreferences(next_preferences, this.system_color_mode);
    this.reading?.cancel();
    this.initialized = true;
    this.preferences = next_preferences;
    this.theme_id = next_theme;
    this.read_error = null;
    this.recovered_unknown = false;
    this.save_error = null;
    this.selection = {
      theme_id: next_theme,
      appearance: next_preferences,
    };
    this.applyAppearance();
    this.pending_save = this.selection;
    this.startSave();
  }

  retrySave(): void {
    if (!this.disposed && !this.saving && this.unsaved) {
      this.reading?.cancel();
      this.save_error = null;
      this.selection = {
        theme_id: this.theme_id,
        appearance: this.persistence.saveAppearance ? this.preferences : null,
      };
      this.pending_save = this.selection;
      this.startSave();
    }
  }

  /** 只等待已发生的保存意图，不自动重试失败；退出流程后续可据 false 提示尚未保存。 */
  async flush(): Promise<boolean> {
    while (this.save_task) await this.save_task;
    return !this.unsaved;
  }

  /** 复用退出门禁；没有显式选择时，读取失败不能阻止关闭，也不能回写默认值。 */
  async flushForExit(): Promise<void> {
    if (!this.selection) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const saved = await Promise.race([
        this.flush(),
        new Promise<boolean>((resolve) => {
          timer = setTimeout(() => resolve(false), 1_500);
        }),
      ]);
      if (!saved) throw new Error("配色尚未保存，请稍后重试或在外观设置中重新保存。");
    } finally {
      clearTimeout(timer);
    }
  }

  retryNativeAppearance(): void {
    if (!this.disposed && !this.applying_native) this.applyAppearance();
  }

  private applyAppearance(): void {
    this.presentation.apply(this.theme_id, this.native_error !== null, this.preferences);
    this.native_selection = { theme_id: this.theme_id };
    this.pending_native = this.native_selection;
    if (!this.applying_native) {
      this.applying_native = true;
      void this.drainNative();
    }
  }

  private async drainNative(): Promise<void> {
    while (!this.disposed && this.pending_native) {
      const selection = this.pending_native;
      this.pending_native = null;
      let error: string | null = null;
      try {
        await this.presentation.applyNative(selection.theme_id);
      } catch {
        error = "窗口外观暂未同步，已使用纯色背景。";
      }
      if (!this.disposed && this.native_selection === selection) {
        runInAction(() => {
          this.native_error = error;
          this.presentation.apply(this.theme_id, error !== null, this.preferences);
        });
      }
    }
    if (!this.disposed) {
      runInAction(() => {
        this.applying_native = false;
      });
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.dispose_media_listener?.();
    this.dispose_media_listener = null;
    this.reading?.cancel();
    this.pending_save = null;
    this.pending_native = null;
    this.applying_native = false;
    this.loading = false;
    this.saving = false;
    // 已发出的原生写入不能撤销；受控退出须先 flush，dispose 后不继续排队或回填状态。
  }

  private startSave(): void {
    if (this.saving || this.disposed) return;
    this.saving = true;
    const task = this.drainSaves();
    this.save_task = task;
    void task.then(() => {
      if (this.save_task === task) this.save_task = null;
    });
  }

  private async drainSaves(): Promise<void> {
    while (!this.disposed && this.pending_save) {
      const selection = this.pending_save;
      this.pending_save = null;
      try {
        const saving_theme = this.persistence.save(selection.theme_id);
        // Web 必须在本次选择内同步落盘，不能拖到 Promise 微任务后才写 localStorage。
        if (saving_theme) await saving_theme;
        if (selection.appearance && this.persistence.saveAppearance) {
          const saving_appearance = this.persistence.saveAppearance(selection.appearance);
          if (saving_appearance) await saving_appearance;
        }
        if (!this.disposed) {
          runInAction(() => {
            this.saved_theme_id = selection.theme_id;
            if (selection.appearance) {
              this.saved_preferences = selection.appearance;
            }
          });
        }
      } catch {
        // A→B→A 的两个 A 也是不同意图；旧失败不污染新选择，最新失败只显示一次并等待重试。
        if (!this.disposed && this.selection === selection) {
          runInAction(() => {
            this.save_error = "已应用，未保存。下次打开可能恢复之前的配色。";
          });
        }
      }
    }
    if (!this.disposed) {
      runInAction(() => {
        this.saving = false;
      });
    }
  }
}
