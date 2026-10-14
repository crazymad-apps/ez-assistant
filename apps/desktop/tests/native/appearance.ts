// 独立原生夹具直接消费产品 owner、主题投影和原生应用命令，不启动 Runtime。
import { invoke } from '@tauri-apps/api/core';
import { AppearanceStore } from '../../src/stores/AppearanceStore';
import { applyDocumentTheme, THEME_PRESETS } from '../../src/styles/themePresets';
import { applyDesktopTheme } from '../../src/native-bridge/desktopPreferences';
import '../../src/styles/tokens.scss';
import '../../src/styles/reset.scss';

const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
const assert = (value: boolean, message: string) => { if (!value) throw new Error(message); };
let fail_native = false;
const store = new AppearanceStore({
  load: () => invoke('probe_load'),
  save: (theme_id) => invoke('probe_save', { themeId: theme_id }),
}, { apply: applyDocumentTheme, applyNative: (id) => fail_native ? Promise.reject(new Error('fixture native failure')) : applyDesktopTheme(id) });

async function settleNative() {
  const deadline = Date.now() + 2_000;
  while (store.applying_native && Date.now() < deadline) await wait(20);
  assert(!store.applying_native, 'native apply did not settle');
}

async function verify() {
  // 留出观察尚未绘制主体时原生启动背景的时间。
  await wait(6_000);
  const second = await invoke<boolean>('probe_round');
  await store.initialize();
  assert(store.theme_id === (second ? 'solarized_dark' : 'monokai'), 'startup did not restore the saved preference');
  const root = document.documentElement;
  const app = document.querySelector<HTMLDivElement>('#app')!;
  app.style.cssText = 'padding:64px; min-height:100vh; background:var(--ez-surface); color:var(--ez-text)';
  app.innerHTML = '<main data-left-sidebar="open" data-platform="macos"><h1>F04 配色验证</h1><p>启动主题已在主体挂载前恢复。</p><p id="current"></p></main>';
  for (const id of ['monokai', 'solarized_light', 'default_light', 'solarized_dark'] as const) {
    store.selectTheme(id);
    assert(root.dataset.theme === id, 'DOM projection is not synchronous');
    assert(root.style.colorScheme === THEME_PRESETS[id].color_mode, 'color scheme mismatch');
    await settleNative();
    assert(!store.native_error && !store.applying_native, 'native apply failed');
    assert(root.classList.contains('dark') === (THEME_PRESETS[id].color_mode === 'dark'), 'dark class mismatch');
    assert(matchMedia('(prefers-color-scheme: dark)').matches === (THEME_PRESETS[id].color_mode === 'dark'), 'native media query mismatch');
    document.querySelector('#current')!.textContent = THEME_PRESETS[id].name;
    await wait(5_000);
  }
  fail_native = true;
  store.retryNativeAppearance();
  await settleNative();
  assert(store.native_error !== null, 'native failure was not reported');
  assert(getComputedStyle(document.body).backgroundColor === getComputedStyle(app).backgroundColor, 'material fallback is not solid');
  fail_native = false;
  store.retryNativeAppearance();
  await settleNative();
  assert(store.native_error === null, 'native retry did not recover');
  await store.flushForExit();
  assert(store.saved_theme_id === 'solarized_dark', 'final theme not saved');
  await invoke('probe_finish', { themeId: store.theme_id });
}

void verify().catch((error) => invoke('probe_fail', { message: String(error) }));
