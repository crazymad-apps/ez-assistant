import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CodeHighlighterPlugin, HighlightResult, ThemeInput } from "@streamdown/code";
import type { PluginConfig } from "streamdown";
import type { ThemePreset } from "../../styles/themePresets";

/** 当前 Markdown 的高亮作用域；旧主题、旧文本及卸载后的回调均不可提交。 */
export function scopedCodePlugin(base: CodeHighlighterPlugin | null, theme: ThemePreset, active: () => boolean): CodeHighlighterPlugin {
  const themes: [ThemeInput, ThemeInput] = [theme.code_theme as ThemeInput, theme.code_theme as ThemeInput];
  return {
    name: "shiki", type: "code-highlighter", getThemes: () => themes,
    getSupportedLanguages: () => base?.getSupportedLanguages() ?? [],
    supportsLanguage: (language) => base?.supportsLanguage(language) ?? false,
    highlight(options, callback) {
      const plain: HighlightResult = {
        fg: theme.colors["--ez-text"], bg: theme.colors["--ez-surface-muted"],
        tokens: options.code.split("\n").map((content) => [{ content, offset: 0, color: theme.colors["--ez-text"], htmlStyle: {} }]),
      };
      const matches = (result: HighlightResult) => result.tokens.map((line) => line.map((token) => token.content).join("")).join("\n") === options.code;
      try {
        const result = base?.highlight({ ...options, themes }, (value) => {
          // 上游缓存按长度及首尾片段取 key，必须同时核对完整原文。
          if (active() && matches(value)) callback?.(value);
        });
        return result && matches(result) ? result : plain;
      } catch { return plain; }
    },
  };
}

let code_module: Promise<typeof import("@streamdown/code")> | null = null;
let math_module: Promise<typeof import("@streamdown/math")> | null = null;

export function useMarkdownPlugins(text: string, theme: ThemePreset): PluginConfig {
  const [code, setCode] = useState<CodeHighlighterPlugin | null>(null);
  const [math, setMath] = useState<PluginConfig["math"]>();
  const needs_code = /(?:```|~~~)/.test(text);
  const needs_math = /(^|[^\\])\$\$?[\s\S]*?\$\$?/.test(text);
  const current = useRef<object | null>(null);
  const scope = useMemo(() => ({}), [text, theme]);
  // 在提交阶段失效；父层 layout effect 先于代码组件的异步高亮 effect。
  useLayoutEffect(() => { current.current = scope; return () => { current.current = null; }; }, [scope]);
  useEffect(() => {
    let active = true;
    if (needs_code) {
      code_module ??= import("@streamdown/code");
      void code_module.then(({ createCodePlugin }) => {
        if (active) setCode(createCodePlugin({ themes: [theme.code_theme as ThemeInput, theme.code_theme as ThemeInput] }));
      }).catch(() => { code_module = null; });
    }
    if (needs_math) {
      math_module ??= import("@streamdown/math");
      void math_module.then(({ math: plugin }) => { if (active) setMath(plugin); }).catch(() => { math_module = null; });
    }
    return () => { active = false; };
  }, [needs_code, needs_math, theme]);
  return useMemo(() => ({ code: scopedCodePlugin(code, theme, () => current.current === scope), math }), [code, theme, scope, math]);
}
