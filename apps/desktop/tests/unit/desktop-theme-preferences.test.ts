import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke, isTauri } from '@tauri-apps/api/core';
import {
  loadDesktopPreferences,
  loadDesktopTheme,
  saveDesktopPreferences,
  saveDesktopTheme,
} from '../../src/native-bridge/desktopPreferences';
import { AppearanceStore } from '../../src/stores/AppearanceStore';
import { isThemeId, THEME_PRESETS, type ThemeId } from '../../src/styles/themePresets';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(), isTauri: vi.fn(() => false) }));

beforeEach(() => {
  vi.restoreAllMocks();
  vi.mocked(invoke).mockReset();
  vi.mocked(isTauri).mockReturnValue(false);
  localStorage.clear();
});

describe('theme storage through existing desktop preferences bridge', () => {
  it('keeps theme independent from old layout snapshots and never copies user information', async () => {
    const old_layout = await loadDesktopPreferences();
    saveDesktopTheme('monokai');
    expect(loadDesktopTheme()).toEqual({ theme_id: 'monokai', recovered_unknown: false });
    await saveDesktopPreferences({ ...old_layout, left_sidebar_width: 340 });
    expect(loadDesktopTheme()).toMatchObject({ theme_id: 'monokai' });
    const layout = localStorage.getItem('ez-assistant:view');
    saveDesktopTheme('solarized_dark');
    expect(localStorage.getItem('ez-assistant:view')).toBe(layout);
    expect(JSON.parse(layout!)).not.toHaveProperty('theme_id');
    expect(localStorage.getItem('ez-assistant:theme')).toBe('solarized_dark');
  });

  it('falls back from unknown values without normalizing storage on reads', () => {
    expect(loadDesktopTheme()).toEqual({ theme_id: 'default_light', recovered_unknown: false });
    expect(localStorage.length).toBe(0);
    for (const value of ['', 'dark', 'null', 'constructor', '__proto__']) {
      localStorage.setItem('ez-assistant:theme', value);
      expect(loadDesktopTheme()).toEqual({ theme_id: 'default_light', recovered_unknown: true });
      expect(localStorage.getItem('ez-assistant:theme')).toBe(value);
      expect(() => saveDesktopTheme(value as ThemeId)).toThrow();
    }
  });

  it('propagates unavailable Web storage and retains the selected theme for retry', async () => {
    const store = new AppearanceStore();
    await store.initialize();
    const set = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('quota');
    });
    store.selectTheme('monokai');
    expect(await store.flush()).toBe(false);
    expect(store.theme_id).toBe('monokai');
    expect(store.save_error).toContain('未保存');
    set.mockRestore();
    store.retrySave();
    expect(await store.flush()).toBe(true);
    expect(localStorage.getItem('ez-assistant:theme')).toBe('monokai');
    store.dispose();
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('denied');
    });
    expect(() => loadDesktopTheme()).toThrow('denied');
  });

  it('uses native field-specific commands without a Host namespace or browser copy', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    vi.mocked(invoke).mockResolvedValueOnce({ theme_id: 'solarized_light', recovered_unknown: false });
    expect(await loadDesktopTheme()).toMatchObject({ theme_id: 'solarized_light' });
    expect(invoke).toHaveBeenNthCalledWith(1, 'load_desktop_theme');
    vi.mocked(invoke).mockResolvedValueOnce(undefined);
    await saveDesktopTheme('monokai');
    expect(invoke).toHaveBeenNthCalledWith(2, 'save_desktop_theme', { themeId: 'monokai' });
    expect(localStorage.length).toBe(0);
    vi.mocked(invoke).mockRejectedValueOnce('SaveFailed');
    await expect(saveDesktopTheme('solarized_dark')).rejects.toBe('SaveFailed');
  });

  it('exposes only immutable built-in metadata and rejects prototype keys', () => {
    expect(Object.keys(THEME_PRESETS)).toHaveLength(4);
    expect(Object.isFrozen(THEME_PRESETS)).toBe(true);
    for (const preset of Object.values(THEME_PRESETS)) {
      expect(Object.isFrozen(preset)).toBe(true);
      expect(preset.startup_background).toMatch(/^#[0-9a-f]{6}$/);
    }
    expect(isThemeId('toString')).toBe(false);
  });
});
