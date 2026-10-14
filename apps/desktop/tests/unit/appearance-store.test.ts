import { afterEach, describe, expect, it, vi } from 'vitest';
import { AppearanceStore } from '../../src/stores/AppearanceStore';
import type { DesktopThemePreference } from '../../src/native-bridge/desktopPreferences';
import { applyDocumentTheme, THEME_PRESETS, type ThemeId } from '../../src/styles/themePresets';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

function preference(theme_id: ThemeId): DesktopThemePreference {
  return { theme_id, recovered_unknown: false };
}

afterEach(() => {
  vi.useRealTimers();
});

describe('application appearance preferences', () => {
  it('normalizes an unknown saved value only when the user explicitly selects the fallback theme', async () => {
    const save = vi.fn();
    const store = new AppearanceStore({
      load: () => ({ theme_id: 'default_light', recovered_unknown: true }),
      save,
    });
    await store.initialize();
    expect(store.recovered_unknown).toBe(true);
    expect(save).not.toHaveBeenCalled();
    expect(await store.flush()).toBe(true);
    expect(save).not.toHaveBeenCalled();
    store.selectTheme('default_light');
    expect(save).toHaveBeenCalledWith('default_light');
    expect(store.recovered_unknown).toBe(false);
    store.dispose();
  });

  it('reads and writes Web preferences synchronously, without saving on initialization', async () => {
    const save = vi.fn();
    const store = new AppearanceStore({ load: () => preference('monokai'), save });
    const loading = store.initialize();
    expect(store.theme_id).toBe('monokai');
    expect(store.preset.color_mode).toBe('dark');
    expect(store.initialized).toBe(true);
    expect(save).not.toHaveBeenCalled();
    await loading;
    store.selectTheme('solarized_light');
    expect(save).toHaveBeenCalledWith('solarized_light');
    expect(store.saved_theme_id).toBe('solarized_light');
    expect(store.unsaved).toBe(false);
    expect(await store.flush()).toBe(true);
    store.selectTheme('solarized_light');
    expect(save).toHaveBeenCalledTimes(1);
    store.dispose();
  });

  it('bounds initialization and ignores late reads without writing fallback values', async () => {
    vi.useFakeTimers();
    const read = deferred<DesktopThemePreference>();
    const load = vi.fn(() => read.promise);
    const save = vi.fn();
    const store = new AppearanceStore({ load, save });
    const loading = store.initialize();
    expect(store.initialize()).toBe(loading);
    await vi.advanceTimersByTimeAsync(1_500);
    await loading;
    expect(store.theme_id).toBe('default_light');
    expect(store.read_error).toContain('超时');
    expect(save).not.toHaveBeenCalled();
    store.selectTheme('solarized_dark');
    read.resolve(preference('monokai'));
    await Promise.resolve();
    expect(store.theme_id).toBe('solarized_dark');
    expect(store.saved_theme_id).toBe('solarized_dark');
    expect(store.read_error).toBeNull();
    expect(load).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
    store.dispose();
  });

  it('keeps explicit selection when an initial read completes later', async () => {
    const read = deferred<DesktopThemePreference>();
    const store = new AppearanceStore({ load: () => read.promise, save: () => undefined });
    const loading = store.initialize();
    store.selectTheme('monokai');
    await loading;
    read.resolve(preference('solarized_light'));
    await Promise.resolve();
    expect(store.theme_id).toBe('monokai');
    expect(store.saved_theme_id).toBe('monokai');
    expect(store.loading).toBe(false);
    store.dispose();
  });

  it('reports read errors without exposing native details or overwriting storage', async () => {
    const save = vi.fn();
    const store = new AppearanceStore({
      load: () => {
        throw new Error('private-config-path');
      },
      save,
    });
    await store.initialize();
    expect(store.read_error).toContain('无法读取');
    expect(store.read_error).not.toContain('private-config-path');
    expect(store.saved_theme_id).toBeNull();
    expect(save).not.toHaveBeenCalled();
    store.dispose();
  });

  it('serializes writes and coalesces only selections that have not started', async () => {
    const first = deferred<void>();
    const second = deferred<void>();
    const save = vi
      .fn<(theme: ThemeId) => Promise<void>>()
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => second.promise);
    const store = new AppearanceStore({ load: () => preference('default_light'), save });
    await store.initialize();
    store.selectTheme('monokai');
    store.selectTheme('solarized_light');
    store.selectTheme('solarized_dark');
    expect(store.theme_id).toBe('solarized_dark');
    expect(save.mock.calls).toEqual([['monokai']]);
    first.resolve();
    await Promise.resolve();
    expect(save.mock.calls).toEqual([['monokai'], ['solarized_dark']]);
    expect(store.saved_theme_id).toBe('monokai');
    expect(store.saving).toBe(true);
    second.resolve();
    expect(await store.flush()).toBe(true);
    expect(store.saved_theme_id).toBe('solarized_dark');
    expect(store.saving).toBe(false);
    store.dispose();
  });

  it('does not attach an old A failure to a newer A in A-B-A, and retries only the current failure', async () => {
    const old_a = deferred<void>();
    const new_a = deferred<void>();
    const save = vi
      .fn<(theme: ThemeId) => void | Promise<void>>()
      .mockImplementationOnce(() => old_a.promise)
      .mockImplementationOnce(() => new_a.promise)
      .mockImplementationOnce(() => undefined);
    const store = new AppearanceStore({ load: () => preference('default_light'), save });
    await store.initialize();
    store.selectTheme('monokai');
    store.selectTheme('solarized_light');
    store.selectTheme('monokai');
    old_a.reject(new Error('old private error'));
    await Promise.resolve();
    expect(store.save_error).toBeNull();
    expect(save.mock.calls).toEqual([['monokai'], ['monokai']]);
    new_a.reject(new Error('new private error'));
    expect(await store.flush()).toBe(false);
    expect(store.theme_id).toBe('monokai');
    expect(store.saved_theme_id).toBe('default_light');
    expect(store.save_error).toContain('未保存');
    expect(store.save_error).not.toContain('private');
    expect(save).toHaveBeenCalledTimes(2);
    store.retrySave();
    expect(await store.flush()).toBe(true);
    expect(store.save_error).toBeNull();
    expect(save.mock.calls).toEqual([['monokai'], ['monokai'], ['monokai']]);
    store.dispose();
  });

  it('saves a return to the original value after another write is already in flight', async () => {
    const first = deferred<void>();
    const save = vi
      .fn<(theme: ThemeId) => void | Promise<void>>()
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => undefined);
    const store = new AppearanceStore({ load: () => preference('default_light'), save });
    await store.initialize();
    store.selectTheme('monokai');
    store.selectTheme('default_light');
    first.resolve();
    expect(await store.flush()).toBe(true);
    expect(save.mock.calls).toEqual([['monokai'], ['default_light']]);
    expect(store.saved_theme_id).toBe('default_light');
    store.dispose();
  });

  it('settles initialization on disposal and ignores its eventual rejection', async () => {
    vi.useFakeTimers();
    const read = deferred<DesktopThemePreference>();
    const save = vi.fn();
    const store = new AppearanceStore({ load: () => read.promise, save });
    const loading = store.initialize();
    store.dispose();
    await loading;
    read.reject(new Error('late'));
    await Promise.resolve();
    store.selectTheme('monokai');
    expect(store.read_error).toBeNull();
    expect(store.loading).toBe(false);
    expect(save).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('does not start a queued save or update state after disposal', async () => {
    const first = deferred<void>();
    const save = vi.fn(() => first.promise);
    const store = new AppearanceStore({ load: () => preference('default_light'), save });
    await store.initialize();
    store.selectTheme('monokai');
    store.selectTheme('solarized_dark');
    store.dispose();
    first.resolve();
    expect(await store.flush()).toBe(false);
    expect(save).toHaveBeenCalledTimes(1);
    expect(store.saved_theme_id).toBe('default_light');
  });
});


describe('appearance projection and exit boundary', () => {
  it('projects the saved theme before initialization returns, including mode and complete colors', async () => {
    const native = vi.fn(async () => undefined);
    const store = new AppearanceStore({ load: () => preference('solarized_dark'), save: vi.fn() }, { apply: applyDocumentTheme, applyNative: native });
    const ready = store.initialize();
    const root = document.documentElement;
    expect(root.dataset.theme).toBe('solarized_dark');
    expect(root.classList.contains('dark')).toBe(true);
    expect(root.style.colorScheme).toBe('dark');
    expect(root.style.getPropertyValue('--ez-surface')).toBe('#002b36');
    await ready;
    store.selectTheme('solarized_light');
    expect(root.classList.contains('dark')).toBe(false);
    expect(root.style.colorScheme).toBe('light');
    for (const preset of Object.values(THEME_PRESETS)) {
      expect(Object.keys(preset.colors).sort()).toEqual(Object.keys(THEME_PRESETS.default_light.colors).sort());
    }
    store.dispose();
  });

  it('serializes native application independently of saving and ignores old A failures in A-B-A', async () => {
    const old_a = deferred<void>();
    const new_a = deferred<void>();
    const native = vi.fn<(id: ThemeId) => Promise<void>>()
      .mockResolvedValueOnce(undefined)
      .mockImplementationOnce(() => old_a.promise)
      .mockImplementationOnce(() => new_a.promise);
    const save = vi.fn();
    const store = new AppearanceStore({ load: () => preference('default_light'), save }, { apply: applyDocumentTheme, applyNative: native });
    await store.initialize();
    store.selectTheme('monokai');
    store.selectTheme('solarized_dark');
    store.selectTheme('monokai');
    expect(await store.flush()).toBe(true);
    expect(native.mock.calls).toEqual([['default_light'], ['monokai']]);
    old_a.reject(new Error('old'));
    await Promise.resolve();
    expect(native.mock.calls).toEqual([['default_light'], ['monokai'], ['monokai']]);
    expect(store.native_error).toBeNull();
    new_a.reject(new Error('private-path'));
    await Promise.resolve();
    expect(store.native_error).toContain('纯色');
    expect(store.native_error).not.toContain('private');
    expect(document.documentElement.dataset.nativeThemeFailed).toBe('true');
    expect(store.save_error).toBeNull();
    expect(store.saved_theme_id).toBe('monokai');
    native.mockResolvedValueOnce(undefined);
    const count = save.mock.calls.length;
    store.retryNativeAppearance();
    await Promise.resolve();
    expect(store.native_error).toBeNull();
    expect(document.documentElement.dataset.nativeThemeFailed).toBe('false');
    expect(save).toHaveBeenCalledTimes(count);
    store.dispose();
  });

  it('does not apply late native results or queued selections after disposal', async () => {
    const pending = deferred<void>();
    const apply = vi.fn();
    const native = vi.fn(() => pending.promise);
    const store = new AppearanceStore({ load: () => preference('monokai'), save: vi.fn() }, { apply, applyNative: native });
    await store.initialize();
    store.selectTheme('solarized_dark');
    store.dispose();
    apply.mockClear();
    pending.reject(new Error('late'));
    await Promise.resolve();
    expect(native).toHaveBeenCalledTimes(1);
    expect(apply).not.toHaveBeenCalled();
    expect(store.native_error).toBeNull();
  });

  it('waits for the actual last saved choice on exit, but bounds an unresponsive write', async () => {
    vi.useFakeTimers();
    const pending = deferred<void>();
    const store = new AppearanceStore({ load: () => preference('default_light'), save: () => pending.promise });
    await store.initialize();
    store.selectTheme('monokai');
    const failed = expect(store.flushForExit()).rejects.toThrow('配色尚未保存');
    await vi.advanceTimersByTimeAsync(1_500);
    await failed;
    expect(store.saving).toBe(true);
    expect(store.saved_theme_id).toBe('default_light');
    const exit = store.flushForExit();
    pending.resolve();
    await expect(exit).resolves.toBeUndefined();
    expect(vi.getTimerCount()).toBe(0);
    store.dispose();
  });

  it('does not block exit or write defaults after a failed initial read without selection', async () => {
    const save = vi.fn();
    const store = new AppearanceStore({ load: () => { throw new Error('read'); }, save });
    await store.initialize();
    await expect(store.flushForExit()).resolves.toBeUndefined();
    expect(save).not.toHaveBeenCalled();
    store.dispose();
  });
});
