import { isTauri } from "@tauri-apps/api/core";
import { observer } from "mobx-react-lite";
import { useEffect, useState, type CSSProperties } from "react";
import { Button } from "../../../../components/Button";
import { Icon, type IconName } from "../../../../components/Icon";
import { SelectionPopover, type SelectionOption } from "../../../../components/SelectionPopover";
import { TextInput } from "../../../../components/TextInput";
import { useAppearance } from "../../../../stores/AppearanceContext";
import {
  ACCENT_SWATCH_OPTIONS,
  CODE_FONT_OPTIONS,
  CONTENT_WIDTH_OPTIONS,
  DENSITY_OPTIONS,
  FONT_SIZE_OPTIONS,
  LINE_HEIGHT_OPTIONS,
  PROSE_FONT_OPTIONS,
  UI_FONT_OPTIONS,
  normalizeHexColor,
  type AppearanceMode,
  type CodeFontId,
  type ContentWidthPreset,
  type DensityPreset,
  type FontSizeScale,
  type LineHeightPreset,
  type ProseFontId,
  type UiFontId,
} from "../../../../styles/appearanceOptions";
import {
  DARK_THEME_IDS,
  LIGHT_THEME_IDS,
  THEME_PRESETS,
  type ThemeId,
} from "../../../../styles/themePresets";
import { SettingsPageContainer } from "../SettingsPageContainer";
import styles from "./index.module.scss";

const MODE_OPTIONS: ReadonlyArray<Readonly<{ value: AppearanceMode; label: string; icon: IconName }>> = [
  { value: "system", label: "跟随系统", icon: "desktop" },
  { value: "light", label: "亮色", icon: "sun" },
  { value: "dark", label: "深色", icon: "moon" },
];

const FONT_SIZE_SELECT_OPTIONS: ReadonlyArray<SelectionOption<FontSizeScale>> = FONT_SIZE_OPTIONS.map((item) => ({
  value: item.value,
  label: item.label,
  description: item.description,
}));

const LINE_HEIGHT_SELECT_OPTIONS: ReadonlyArray<SelectionOption<LineHeightPreset>> = LINE_HEIGHT_OPTIONS.map((item) => ({
  value: item.value,
  label: item.label,
  description: item.description,
}));

const UI_FONT_SELECT_OPTIONS: ReadonlyArray<SelectionOption<UiFontId>> = UI_FONT_OPTIONS.map((item) => ({
  value: item.value,
  label: item.label,
  description: item.description,
  icon: <span className={styles.font_glyph} style={{ fontFamily: item.stack }}>Aa</span>,
}));

const PROSE_FONT_SELECT_OPTIONS: ReadonlyArray<SelectionOption<ProseFontId>> = PROSE_FONT_OPTIONS.map((item) => ({
  value: item.value,
  label: item.label,
  description: item.description,
  icon: <span className={styles.font_glyph} style={item.stack ? { fontFamily: item.stack } : undefined}>文</span>,
}));

const CODE_FONT_SELECT_OPTIONS: ReadonlyArray<SelectionOption<CodeFontId>> = CODE_FONT_OPTIONS.map((item) => ({
  value: item.value,
  label: item.label,
  description: item.description,
  icon: <span className={styles.font_glyph} style={{ fontFamily: item.stack }}>&lt;/&gt;</span>,
}));

type SwitchRowProps = Readonly<{
  checked: boolean;
  description: string;
  label: string;
  on_change: (next: boolean) => void;
}>;

function SwitchRow(props: SwitchRowProps) {
  return (
    <div className={styles.row_block}>
      <div className={styles.row}>
        <div className={styles.row_label}>
          <strong>{props.label}</strong>
          <span>{props.description}</span>
        </div>
        <div className={styles.row_control}>
          <button
            aria-checked={props.checked}
            aria-label={props.label}
            className={styles.toggle}
            onClick={() => props.on_change(!props.checked)}
            role="switch"
            type="button"
          >
            <span />
          </button>
        </div>
      </div>
    </div>
  );
}

function ThemeCard(props: Readonly<{
  checked: boolean;
  id: ThemeId;
  on_select: (id: ThemeId) => void;
}>) {
  const preset = THEME_PRESETS[props.id];
  const preview = {
    "--preview-background": preset.colors["--ez-surface"],
    "--preview-panel": preset.colors["--ez-sidebar"],
    "--preview-muted": preset.colors["--ez-surface-muted"],
    "--preview-text": preset.colors["--ez-text"],
    "--preview-secondary": preset.colors["--ez-text-secondary"],
    "--preview-accent": preset.colors["--ez-primary-text"],
    "--preview-keyword": preset.syntax.keyword,
    "--preview-string": preset.syntax.string,
  } as CSSProperties;

  return (
    <label className={styles.card}>
      <div aria-hidden="true" className={styles.preview} style={preview}>
        <div className={styles.sidebar}>
          <i />
          <i />
          <i />
        </div>
        <div className={styles.workspace}>
          <b>工作区对话</b>
          <code>
            <em>let</em> ui = <strong>&quot;{props.id}&quot;</strong>;
          </code>
        </div>
      </div>
      <span className={styles.caption}>
        <input
          checked={props.checked}
          name="appearance-theme"
          onChange={() => props.on_select(props.id)}
          type="radio"
          value={props.id}
        />
        <span>{preset.name}</span>
        <small aria-hidden="true">{preset.color_mode === "dark" ? "深色" : "浅色"}</small>
      </span>
    </label>
  );
}

export const AppearanceSettingsPage = observer(function AppearanceSettingsPage() {
  const appearance = useAppearance();
  const prefs = appearance.preferences;
  const active_preset = appearance.preset;

  const [font_size_open, setFontSizeOpen] = useState(false);
  const [ui_font_open, setUiFontOpen] = useState(false);
  const [line_height_open, setLineHeightOpen] = useState(false);
  const [prose_font_open, setProseFontOpen] = useState(false);
  const [code_font_open, setCodeFontOpen] = useState(false);
  const [hex_draft, setHexDraft] = useState(prefs.custom_accent_hex);

  useEffect(() => {
    setHexDraft(prefs.custom_accent_hex);
  }, [prefs.custom_accent_hex]);

  const preview_code_style = {
    "--preview-keyword": active_preset.syntax.keyword,
    "--preview-string": active_preset.syntax.string,
  } as CSSProperties;

  return (
    <SettingsPageContainer
      actions={
        <Button size="small" variant="text" onClick={() => appearance.resetAppearance()}>
          恢复默认
        </Button>
      }
      title="外观"
    >
      <div className={styles.root}>
        <div aria-label="外观效果实时预览" className={styles.preview_banner}>
          <div className={styles.preview_rail}>
            <div className={styles.preview_rail_active}>
              <span>助手对话</span>
              {prefs.show_sidebar_time ? <time>14:20</time> : null}
            </div>
            <div className={styles.preview_rail_item}>
              <span>固件调试</span>
              {prefs.show_sidebar_time ? <time>昨天</time> : null}
            </div>
            <div className={styles.preview_rail_item}>
              <span>代码审查</span>
              {prefs.show_sidebar_time ? <time>周一</time> : null}
            </div>
          </div>
          <div className={styles.preview_stage}>
            <div className={styles.preview_meta}>
              <span>{active_preset.name}</span>
              {prefs.show_message_timestamps ? <time>刚刚</time> : null}
            </div>
            <div className={styles.preview_prose_wrap}>
              <p>实时预览强调色、正文字体、字号与行高排版效果。Welcome to EZ Assistant.</p>
            </div>
            <pre className={styles.preview_code} style={preview_code_style}>
              <code>
                <em>const</em> answer = () =&gt; <strong>&quot;!= ==&gt; 0xFF&quot;</strong>;
              </code>
            </pre>
          </div>
        </div>

        <section className={styles.group}>
          <h4 className={styles.group_title}>通用</h4>
          <div className={styles.rows}>
            <div className={styles.row_block}>
              <div className={styles.row}>
                <div className={styles.row_label}>
                  <strong>主题</strong>
                  <span>调整应用的整体明暗模式</span>
                </div>
                <div className={styles.row_control}>
                  <div aria-label="外观模式" className={styles.segmented} role="group">
                    {MODE_OPTIONS.map((option) => (
                      <button
                        aria-pressed={prefs.mode === option.value}
                        className={styles.segment}
                        key={option.value}
                        onClick={() => appearance.setMode(option.value)}
                        type="button"
                      >
                        <Icon name={option.icon} size={14} />
                        <span>{option.label}</span>
                      </button>
                    ))}
                  </div>
                </div>
              </div>
            </div>

            <div className={styles.row_block}>
              <div className={styles.row}>
                <div className={styles.row_label}>
                  <strong>强调色</strong>
                  <span>选择应用界面的核心高亮与按钮色彩</span>
                </div>
                <div className={styles.row_control}>
                  <div aria-label="强调色" className={styles.swatches} role="radiogroup">
                    {ACCENT_SWATCH_OPTIONS.map((swatch) => {
                      const selected = prefs.accent_preset === swatch.id;
                      return (
                        <label className={styles.swatch} key={swatch.id} title={swatch.label}>
                          <input
                            aria-label={swatch.label}
                            checked={selected}
                            name="appearance-accent"
                            onChange={() => appearance.updatePreferences({ accent_preset: swatch.id })}
                            type="radio"
                            value={swatch.id}
                          />
                          <span
                            aria-hidden="true"
                            className={styles.swatch_dot}
                            data-selected={selected}
                            data-theme-default={swatch.id === "theme"}
                            style={swatch.hex ? { backgroundColor: swatch.hex } : undefined}
                          >
                            {selected ? <Icon name="check" size={13} /> : null}
                          </span>
                        </label>
                      );
                    })}
                    <label className={styles.swatch} title="自定义强调色">
                      <input
                        aria-label="自定义强调色"
                        checked={prefs.accent_preset === "custom"}
                        name="appearance-accent"
                        onChange={() => appearance.updatePreferences({ accent_preset: "custom" })}
                        type="radio"
                        value="custom"
                      />
                      <span
                        aria-hidden="true"
                        className={styles.swatch_dot}
                        data-rainbow="true"
                        data-selected={prefs.accent_preset === "custom"}
                      >
                        <Icon name={prefs.accent_preset === "custom" ? "check" : "plus"} size={13} />
                      </span>
                    </label>
                  </div>
                </div>
              </div>
              {prefs.accent_preset === "custom" ? (
                <div className={styles.custom_row}>
                  <span className={styles.scope}>选择任意强调色（自动适配当前亮暗文字对比度）</span>
                  <div className={styles.custom_color_controls}>
                    <input
                      aria-label="取色器"
                      className={styles.color_picker}
                      onChange={(event) => {
                        const value = event.target.value;
                        setHexDraft(value);
                        appearance.updatePreferences({ accent_preset: "custom", custom_accent_hex: value });
                      }}
                      type="color"
                      value={prefs.custom_accent_hex}
                    />
                    <TextInput
                      aria-label="自定义强调色十六进制"
                      className={styles.hex_input}
                      onBlur={() => {
                        const normalized = normalizeHexColor(hex_draft);
                        if (normalized) {
                          setHexDraft(normalized);
                          appearance.updatePreferences({
                            accent_preset: "custom",
                            custom_accent_hex: normalized,
                          });
                        } else {
                          setHexDraft(prefs.custom_accent_hex);
                        }
                      }}
                      onChange={(event) => {
                        const raw = event.target.value;
                        setHexDraft(raw);
                        const trimmed = raw.trim().toLowerCase();
                        if (/^#[0-9a-f]{6}$/.test(trimmed)) {
                          appearance.updatePreferences({
                            accent_preset: "custom",
                            custom_accent_hex: trimmed,
                          });
                        }
                      }}
                      placeholder="#f97316"
                      value={hex_draft}
                    />
                  </div>
                </div>
              ) : null}
            </div>

            <div className={styles.row_block}>
              <div className={styles.row}>
                <div className={styles.row_label}>
                  <strong>全局字体大小</strong>
                  <span>同步缩放界面文字、侧栏与消息阅读基准字号</span>
                </div>
                <div className={styles.row_control}>
                  <div className={styles.select_box}>
                    <SelectionPopover
                      aria_label="全局字体大小"
                      on_open_change={setFontSizeOpen}
                      on_select={(font_size) => appearance.updatePreferences({ font_size })}
                      open={font_size_open}
                      options={FONT_SIZE_SELECT_OPTIONS}
                      selected={prefs.font_size}
                      trigger_variant="field"
                    />
                  </div>
                </div>
              </div>
            </div>

            <div className={styles.row_block}>
              <div className={styles.row}>
                <div className={styles.row_label}>
                  <strong>界面字体</strong>
                  <span>调整用于侧栏、按钮与弹窗等界面元素的字体</span>
                </div>
                <div className={styles.row_control}>
                  <div className={styles.select_box}>
                    <SelectionPopover
                      aria_label="界面字体"
                      on_open_change={setUiFontOpen}
                      on_select={(ui_font) => appearance.updatePreferences({ ui_font })}
                      open={ui_font_open}
                      options={UI_FONT_SELECT_OPTIONS}
                      selected={prefs.ui_font}
                      trigger_variant="field"
                    />
                  </div>
                </div>
              </div>
              {prefs.ui_font === "custom" ? (
                <div className={styles.custom_row}>
                  <span className={styles.scope}>填写本机已安装的字体名称（如 MiSans / Sarasa Gothic SC）</span>
                  <TextInput
                    aria-label="自定义界面字体"
                    className={styles.custom_font_input}
                    onChange={(event) => appearance.updatePreferences({ custom_ui_font: event.target.value })}
                    placeholder="例如 PingFang SC"
                    value={prefs.custom_ui_font}
                  />
                </div>
              ) : null}
            </div>

            <div className={styles.row_block}>
              <div className={styles.row}>
                <div className={styles.row_label}>
                  <strong>正文行高</strong>
                  <span>调整对话正文与 Markdown 文本行间距</span>
                </div>
                <div className={styles.row_control}>
                  <div className={styles.select_box}>
                    <SelectionPopover
                      aria_label="正文行高"
                      on_open_change={setLineHeightOpen}
                      on_select={(line_height) => appearance.updatePreferences({ line_height })}
                      open={line_height_open}
                      options={LINE_HEIGHT_SELECT_OPTIONS}
                      selected={prefs.line_height}
                      trigger_variant="field"
                    />
                  </div>
                </div>
              </div>
            </div>
          </div>
        </section>

        <fieldset className={styles.options}>
          <legend>配色方案</legend>
          <p className={styles.theme_subhead}>浅色主题</p>
          <div className={styles.cards}>
            {LIGHT_THEME_IDS.map((id) => (
              <ThemeCard
                checked={appearance.theme_id === id}
                id={id}
                key={id}
                on_select={(selected) => appearance.selectTheme(selected)}
              />
            ))}
          </div>
          <p className={styles.theme_subhead}>深色主题</p>
          <div className={styles.cards}>
            {DARK_THEME_IDS.map((id) => (
              <ThemeCard
                checked={appearance.theme_id === id}
                id={id}
                key={id}
                on_select={(selected) => appearance.selectTheme(selected)}
              />
            ))}
          </div>
        </fieldset>

        <section className={styles.group}>
          <h4 className={styles.group_title}>排版</h4>
          <div className={styles.rows}>
            <div className={styles.row_block}>
              <div className={styles.row}>
                <div className={styles.row_label}>
                  <strong>正文字体</strong>
                  <span>独立设置对话阅读与文档内容使用的字体</span>
                </div>
                <div className={styles.row_control}>
                  <div className={styles.select_box}>
                    <SelectionPopover
                      aria_label="正文字体"
                      on_open_change={setProseFontOpen}
                      on_select={(prose_font) => appearance.updatePreferences({ prose_font })}
                      open={prose_font_open}
                      options={PROSE_FONT_SELECT_OPTIONS}
                      selected={prefs.prose_font}
                      trigger_variant="field"
                    />
                  </div>
                </div>
              </div>
              {prefs.prose_font === "custom" ? (
                <div className={styles.custom_row}>
                  <span className={styles.scope}>填写用于阅读正文的本机字体名</span>
                  <TextInput
                    aria-label="自定义正文字体"
                    className={styles.custom_font_input}
                    onChange={(event) => appearance.updatePreferences({ custom_prose_font: event.target.value })}
                    placeholder="例如 LXGW WenKai"
                    value={prefs.custom_prose_font}
                  />
                </div>
              ) : null}
            </div>

            <div className={styles.row_block}>
              <div className={styles.row}>
                <div className={styles.row_label}>
                  <strong>代码字体</strong>
                  <span>调整代码块、Monaco 编辑器与工具输出的等宽字体</span>
                </div>
                <div className={styles.row_control}>
                  <div className={styles.select_box}>
                    <SelectionPopover
                      aria_label="代码字体"
                      on_open_change={setCodeFontOpen}
                      on_select={(code_font) => appearance.updatePreferences({ code_font })}
                      open={code_font_open}
                      options={CODE_FONT_SELECT_OPTIONS}
                      selected={prefs.code_font}
                      trigger_variant="field"
                    />
                  </div>
                </div>
              </div>
              {prefs.code_font === "custom" ? (
                <div className={styles.custom_row}>
                  <span className={styles.scope}>填写本机已安装的等宽字体名（如 Maple Mono NF CN）</span>
                  <TextInput
                    aria-label="自定义代码字体"
                    className={styles.custom_font_input}
                    onChange={(event) => appearance.updatePreferences({ custom_code_font: event.target.value })}
                    placeholder="例如 JetBrains Mono"
                    value={prefs.custom_code_font}
                  />
                </div>
              ) : null}
            </div>

            <SwitchRow
              checked={prefs.code_ligatures}
              description="在支持连字的等宽字体中组合渲染 !=、==>、=> 等编程符号"
              label="代码字体连字"
              on_change={(code_ligatures) => appearance.updatePreferences({ code_ligatures })}
            />
          </div>
        </section>

        <section className={styles.group}>
          <h4 className={styles.group_title}>内容显示</h4>
          <div className={styles.rows}>
            <div className={styles.row_block}>
              <div className={styles.row}>
                <div className={styles.row_label}>
                  <strong>对话阅读宽度</strong>
                  <span>控制消息区域与输入框的最大横向宽度</span>
                </div>
                <div className={styles.row_control}>
                  <div aria-label="对话阅读宽度" className={styles.segmented} role="group">
                    {CONTENT_WIDTH_OPTIONS.map((option) => (
                      <button
                        aria-pressed={prefs.content_width === option.value}
                        className={styles.segment}
                        key={option.value}
                        onClick={() => appearance.updatePreferences({ content_width: option.value as ContentWidthPreset })}
                        type="button"
                      >
                        {option.label}
                      </button>
                    ))}
                  </div>
                </div>
              </div>
            </div>

            <div className={styles.row_block}>
              <div className={styles.row}>
                <div className={styles.row_label}>
                  <strong>界面密度</strong>
                  <span>调整对话边缘留白与上下呼吸感间距</span>
                </div>
                <div className={styles.row_control}>
                  <div aria-label="界面密度" className={styles.segmented} role="group">
                    {DENSITY_OPTIONS.map((option) => (
                      <button
                        aria-pressed={prefs.density === option.value}
                        className={styles.segment}
                        key={option.value}
                        onClick={() => appearance.updatePreferences({ density: option.value as DensityPreset })}
                        type="button"
                      >
                        {option.label}
                      </button>
                    ))}
                  </div>
                </div>
              </div>
            </div>

            <SwitchRow
              checked={prefs.show_sidebar_time}
              description="在左侧会话列表行尾显示最近活动时间"
              label="侧栏会话时间"
              on_change={(show_sidebar_time) => appearance.updatePreferences({ show_sidebar_time })}
            />

            <SwitchRow
              checked={prefs.show_message_timestamps}
              description="在对话消息操作区显示消息发送时间"
              label="消息时间戳"
              on_change={(show_message_timestamps) => appearance.updatePreferences({ show_message_timestamps })}
            />

            {isTauri() ? (
              <SwitchRow
                checked={prefs.macos_vibrancy}
                description="在 macOS 桌面客户端保留侧栏底层毛玻璃透出材质"
                label="窗口透明材质"
                on_change={(macos_vibrancy) => appearance.updatePreferences({ macos_vibrancy })}
              />
            ) : null}
          </div>
        </section>

        <footer className={styles.footer}>
          <p className={styles.scope}>{isTauri() ? "仅此桌面客户端" : "仅此浏览器的当前站点"}</p>
          <div aria-live="polite" className={styles.feedback} role="status">
            {appearance.saving ? (
              "正在保存…"
            ) : appearance.save_error ? (
              <>
                <span>{appearance.save_error}</span>
                <Button size="small" onClick={() => appearance.retrySave()}>
                  重试
                </Button>
              </>
            ) : null}
          </div>
          {appearance.native_error ? (
            <div className={styles.feedback} role="status">
              <span>{appearance.unsaved ? "窗口外观未更新，已使用纯色背景。" : "配色已保存，窗口外观未更新。"}</span>
              <Button disabled={appearance.applying_native} size="small" onClick={() => appearance.retryNativeAppearance()}>
                重试窗口外观
              </Button>
            </div>
          ) : null}
          {appearance.read_error ? <p className={styles.scope}>{appearance.read_error}</p> : null}
          {appearance.recovered_unknown ? <p className={styles.scope}>已保存的配色不可用，已使用默认配色。</p> : null}
        </footer>
      </div>
    </SettingsPageContainer>
  );
});
