import { useEffect, useRef } from "react";
import { createRoot } from "react-dom/client";
import { reaction } from "mobx";
import { observer } from "mobx-react-lite";
import { AppearanceStore } from "../../../src/stores/AppearanceStore";
import { AppearanceProvider } from "../../../src/stores/AppearanceContext";
import { THEME_PRESETS, applyDocumentTheme, type ThemeId } from "../../../src/styles/themePresets";
import { MarkdownContent } from "../../../src/components/MarkdownContent";
import { MonacoTextViewer } from "../../../src/features/resource-workspace/MonacoTextViewer";
import { applyMonacoTheme } from "../../../src/features/resource-workspace/monacoThemes";
import { createTerminalEmulator, type TerminalEmulator } from "../../../src/features/resource-workspace/terminalEmulator";
import "../../../src/styles/index.scss";
import styles from "./index.module.scss";

// 无 Host／PTY：实际 renderer 的独立布局、异步与状态保留验证。
const appearance = new AppearanceStore({ load: () => ({ theme_id: "default_light", recovered_unknown: false }), save: () => undefined }, { apply: applyDocumentTheme, applyNative: async () => undefined });
await appearance.initialize();
const dispose_theme = reaction(() => appearance.theme_id, applyMonacoTheme);
const text = '## 配色预览\n\n```typescript\nconst greeting: string = "hello";\nconsole.log(greeting);\n```\n\n```unknown-language\nplain text remains readable\n```\n\n```mermaid\ngraph LR\nA[读取配置] --> B[应用配色]\nB --> C[保留内容]\n```';
const code = Array.from({ length: 100 }, (_, i) => `const line${i} = "value ${i}";`).join("\n");
const Fixture = observer(function Fixture() {
  const terminal_host = useRef<HTMLDivElement>(null);
  useEffect(() => {
    let disposed = false; let emulator: TerminalEmulator | null = null;
    void createTerminalEmulator().then((value) => {
      if (disposed) { value.terminal.dispose(); return; }
      emulator = value; terminal_host.current!.append(value.host); value.fit.fit();
      value.terminal.write("history stays intact\r\n\x1b[31mred\x1b[0m \x1b[32mgreen\x1b[0m \x1b[34mblue\x1b[0m\r\n\x1b[38;2;190;90;140mtruecolor\x1b[0m", () => value.terminal.select(0, 0, 7));
      (window as Window & { rendererTerminal?: TerminalEmulator }).rendererTerminal = value;
    });
    return () => { disposed = true; emulator?.terminal.dispose(); };
  }, []);
  return <main className={styles.fixture}>
    <nav>{(Object.keys(THEME_PRESETS) as ThemeId[]).map((id) => <button key={id} onClick={() => appearance.selectTheme(id)}>{THEME_PRESETS[id].name}</button>)}</nav>
    <MarkdownContent text={text} theme={appearance.preset} />
    <section className={styles.editor}><MonacoTextViewer resource_key="theme-preview" file_name="sample.ts" initial_line={50} text={code} word_wrap={false} /></section>
    <section className={styles.terminal} ref={terminal_host} />
  </main>;
});
createRoot(document.getElementById("app")!).render(<AppearanceProvider store={appearance}><Fixture /></AppearanceProvider>);
window.addEventListener("pagehide", () => { dispose_theme(); appearance.dispose(); }, { once: true });
