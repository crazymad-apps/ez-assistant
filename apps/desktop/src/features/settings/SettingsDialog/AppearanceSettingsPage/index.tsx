import { isTauri } from '@tauri-apps/api/core';
import { observer } from 'mobx-react-lite';
import type { CSSProperties } from 'react';
import { Button } from '../../../../components/Button';
import { useAppearance } from '../../../../stores/AppearanceContext';
import { THEME_PRESETS, type ThemeId } from '../../../../styles/themePresets';
import { SettingsPageContainer } from '../SettingsPageContainer';
import styles from './index.module.scss';

const themes = Object.keys(THEME_PRESETS) as ThemeId[];

export const AppearanceSettingsPage = observer(function AppearanceSettingsPage() {
  const appearance = useAppearance();
  return <SettingsPageContainer title="外观">
    <fieldset className={styles.options}>
      <legend>配色方案</legend>
      <div className={styles.cards}>
        {themes.map((id) => {
          const preset = THEME_PRESETS[id];
          const preview = {
            '--preview-background': preset.colors['--ez-surface'],
            '--preview-panel': preset.colors['--ez-sidebar'],
            '--preview-muted': preset.colors['--ez-surface-muted'],
            '--preview-text': preset.colors['--ez-text'],
            '--preview-secondary': preset.colors['--ez-text-secondary'],
            '--preview-accent': preset.colors['--ez-primary-text'],
            '--preview-keyword': preset.syntax.keyword,
            '--preview-string': preset.syntax.string,
          } as CSSProperties;
          return <label className={styles.card} key={id}>
            <div className={styles.preview} style={preview} aria-hidden="true">
              <div className={styles.sidebar}><i /><i /><i /></div>
              <div className={styles.workspace}><b>开始今天的工作</b><span>所有内容，保持原样</span>
                <code><em>const</em> theme = <strong>"{id === 'default_light' ? 'light' : id.split('_')[0]}"</strong>;<br /><span>› 准备就绪</span></code>
              </div>
            </div>
            <span className={styles.caption}>
              <input type="radio" name="appearance-theme" value={id} checked={appearance.theme_id === id} onChange={() => appearance.selectTheme(id)} />
              <span>{preset.name}</span>
              <small aria-hidden="true">{preset.color_mode === 'dark' ? '深色' : '浅色'}</small>
            </span>
          </label>;
        })}
      </div>
    </fieldset>
    <p className={styles.scope}>{isTauri() ? '仅此桌面客户端' : '仅此浏览器的当前站点'}</p>
    <div className={styles.feedback} role="status" aria-live="polite">
      {appearance.saving ? '正在保存…' : appearance.save_error && <><span>{appearance.save_error}</span><Button size="small" onClick={() => appearance.retrySave()}>重试</Button></>}
    </div>
    {appearance.native_error && <div className={styles.feedback} role="status">
      <span>{appearance.unsaved ? '窗口外观未更新，已使用纯色背景。' : '配色已保存，窗口外观未更新。'}</span>
      <Button size="small" disabled={appearance.applying_native} onClick={() => appearance.retryNativeAppearance()}>重试窗口外观</Button>
    </div>}
    {appearance.read_error && <p className={styles.scope}>{appearance.read_error}</p>}
    {appearance.recovered_unknown && <p className={styles.scope}>已保存的配色不可用，已使用默认配色。</p>}
  </SettingsPageContainer>;
});
