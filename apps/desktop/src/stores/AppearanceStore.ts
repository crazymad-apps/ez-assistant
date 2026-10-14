import { action, computed, makeObservable, observable, runInAction } from 'mobx';
import { applyDesktopTheme, loadDesktopTheme, saveDesktopTheme, type DesktopThemePreference } from '../native-bridge/desktopPreferences';
import { applyDocumentTheme, DEFAULT_THEME_ID, isThemeId, THEME_PRESETS, type ThemeId } from '../styles/themePresets';

type AppearancePersistence = {
  readonly load: () => DesktopThemePreference | Promise<DesktopThemePreference>;
  readonly save: (theme_id: ThemeId) => void | Promise<void>;
};

type AppearancePresentation = {
  readonly apply: (theme_id: ThemeId, native_failed: boolean) => void;
  readonly applyNative: (theme_id: ThemeId) => Promise<void>;
};

type ThemeSelection = { readonly theme_id: ThemeId };

/** 应用级展示偏好，不依附可替换的 RootStore；DOM 与原生窗口接入由应用装配负责。 */
export class AppearanceStore {
  theme_id: ThemeId = DEFAULT_THEME_ID;
  saved_theme_id: ThemeId | null = null;
  initialized = false;
  loading = false;
  saving = false;
  recovered_unknown = false;
  read_error: string | null = null;
  save_error: string | null = null;

  native_error: string | null = null;
  applying_native = false;
  private native_selection: ThemeSelection | null = null;
  private pending_native: ThemeSelection | null = null;

  private disposed = false;
  private selection: ThemeSelection | null = null;
  private pending_save: ThemeSelection | null = null;
  private save_task: Promise<void> | null = null;
  private initializing: Promise<void> | null = null;
  private reading: { cancel: () => void } | null = null;

  constructor(
    private readonly persistence: AppearancePersistence = {
      load: loadDesktopTheme,
      save: saveDesktopTheme,
    },
    private readonly presentation: AppearancePresentation = {
      apply: applyDocumentTheme,
      applyNative: applyDesktopTheme,
    },
  ) {
    makeObservable(this, {
      theme_id: observable,
      saved_theme_id: observable,
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
      retrySave: action,
      dispose: action,
    });
  }

  get preset() {
    return THEME_PRESETS[this.theme_id];
  }

  get unsaved(): boolean {
    return this.theme_id !== this.saved_theme_id || this.save_error !== null;
  }

  initialize(): Promise<void> {
    if (this.disposed || this.initialized) return Promise.resolve();
    if (this.initializing) return this.initializing;
    this.loading = true;

    this.initializing = new Promise<void>((resolve) => {
      let timer: ReturnType<typeof setTimeout>;
      const reading = { cancel: () => finish() };
      const finish = (value?: DesktopThemePreference, error?: string) => {
        if (this.reading !== reading) return;
        this.reading = null;
        clearTimeout(timer);
        if (!this.disposed) {
          runInAction(() => {
            this.loading = false;
            this.initialized = true;
            this.read_error = error ?? null;
            if (value) {
              this.theme_id = value.theme_id;
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
      timer = setTimeout(() => finish(undefined, '读取配色超时，已使用默认配色。'), 1_500);
      try {
        const loaded = this.persistence.load();
        if ('then' in loaded) {
          void loaded.then(
            (value) => finish(value),
            () => finish(undefined, '无法读取配色，已使用默认配色。'),
          );
        } else {
          // localStorage 同步完成，调用方可以在挂载首屏前直接取得主题。
          finish(loaded);
        }
      } catch {
        finish(undefined, '无法读取配色，已使用默认配色。');
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
    )
      return;

    // 明确选择优先于尚未完成的启动读取，也让等待初始化的调用方立即继续。
    this.reading?.cancel();
    this.initialized = true;
    this.theme_id = theme_id;
    this.read_error = null;
    this.recovered_unknown = false;
    this.save_error = null;
    this.selection = { theme_id };
    this.applyAppearance();
    this.pending_save = this.selection;
    this.startSave();
  }

  retrySave(): void {
    if (!this.disposed && !this.saving && this.unsaved) this.selectTheme(this.theme_id);
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
        new Promise<boolean>((resolve) => { timer = setTimeout(() => resolve(false), 1_500); }),
      ]);
      if (!saved) throw new Error('配色尚未保存，请稍后重试或在外观设置中重新保存。');
    } finally {
      clearTimeout(timer);
    }
  }

  retryNativeAppearance(): void {
    if (!this.disposed && !this.applying_native) this.applyAppearance();
  }

  private applyAppearance(): void {
    this.presentation.apply(this.theme_id, this.native_error !== null);
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
        error = '窗口外观暂未同步，已使用纯色背景。';
      }
      if (!this.disposed && this.native_selection === selection) {
        runInAction(() => {
          this.native_error = error;
          this.presentation.apply(this.theme_id, error !== null);
        });
      }
    }
    if (!this.disposed) runInAction(() => { this.applying_native = false; });
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
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
        const saving = this.persistence.save(selection.theme_id);
        // Web 必须在本次选择内同步落盘，不能拖到 Promise 微任务后才写 localStorage。
        if (saving) await saving;
        if (!this.disposed)
          runInAction(() => {
            this.saved_theme_id = selection.theme_id;
          });
      } catch {
        // A→B→A 的两个 A 也是不同意图；旧失败不污染新选择，最新失败只显示一次并等待重试。
        if (!this.disposed && this.selection === selection) {
          runInAction(() => {
            this.save_error = '已应用，未保存。下次打开可能恢复之前的配色。';
          });
        }
      }
    }
    if (!this.disposed)
      runInAction(() => {
        this.saving = false;
      });
  }
}
