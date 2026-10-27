import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { CodeBlock, CodeBlockCopyButton, Streamdown, type Components, type PluginConfig } from 'streamdown';
import type { CodeHighlighterPlugin, HighlightResult } from '@streamdown/code';
import 'streamdown/styles.css';

let highlighter: Promise<CodeHighlighterPlugin> | undefined;

/** 复用 Agent 的 Streamdown／Shiki 代码块，只接收代码文本，不装配本地文件能力。 */
export function SiteCodeBlock({ code, language }: Readonly<{ code: string; language: string }>) {
  const [base, setBase] = useState<CodeHighlighterPlugin | null>(null);
  const current = useRef<object | null>(null);
  const scope = useMemo(() => ({}), [code, language]);
  useLayoutEffect(() => {
    current.current = scope;
    return () => {
      current.current = null;
    };
  }, [scope]);
  useEffect(() => {
    let active = true;
    highlighter ??= import('@streamdown/code').then(({ createCodePlugin }) =>
      createCodePlugin({ themes: ['github-light', 'github-light'] }),
    );
    void highlighter
      .then((plugin) => {
        if (active) setBase(plugin);
      })
      .catch(() => {
        highlighter = undefined;
      });
    return () => {
      active = false;
    };
  }, []);
  const plugins = useMemo<PluginConfig>(() => {
    return {
      code: {
        name: 'shiki',
        type: 'code-highlighter',
        getThemes: () => ['github-light', 'github-light'],
        getSupportedLanguages: () => base?.getSupportedLanguages() ?? [],
        supportsLanguage: (value) => base?.supportsLanguage(value) ?? false,
        highlight(options, callback) {
          const plain: HighlightResult = {
            fg: '#26303c',
            bg: '#f5f8fc',
            tokens: options.code
              .split('\n')
              .map((content) => [{ content, offset: 0, color: '#26303c', htmlStyle: {} }]),
          };
          const matches = (result: HighlightResult) =>
            result.tokens.map((line) => line.map((token) => token.content).join('')).join('\n') === options.code;
          try {
            const result = base?.highlight(options, (value) => {
              // 与 Agent 一样核对完整原文，避免长度相同代码命中上游旧缓存。
              if (current.current === scope && matches(value)) callback?.(value);
            });
            return result && matches(result) ? result : plain;
          } catch {
            return plain;
          }
        },
      },
    };
  }, [base, code, scope]);
  const components = useMemo<Components>(
    () => ({
      code: () => (
        <CodeBlock code={code} language={language} lineNumbers={false}>
          <CodeBlockCopyButton aria-label="复制代码" />
        </CodeBlock>
      ),
    }),
    [code, language],
  );
  // fence 长于正文中的全部反引号；代码中的 Markdown／HTML 仍然只作为代码文本。
  let fenceLength = 3;
  for (const match of code.matchAll(/`+/g)) fenceLength = Math.max(fenceLength, match[0].length + 1);
  const fence = '`'.repeat(fenceLength);
  const label = /^[a-zA-Z0-9][a-zA-Z0-9_+.-]{0,63}$/.test(language) ? language : '';
  return (
    <Streamdown
      mode="static"
      isAnimating={false}
      lineNumbers={false}
      parseIncompleteMarkdown={false}
      skipHtml
      urlTransform={() => null}
      plugins={plugins}
      components={components}
      controls={{ code: { copy: true, download: false }, table: false, mermaid: false }}
    >
      {`${fence}${label}\n${code}\n${fence}`}
    </Streamdown>
  );
}
