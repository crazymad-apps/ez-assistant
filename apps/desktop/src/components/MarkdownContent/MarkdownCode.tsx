import { createContext, isValidElement, useContext, useEffect, useLayoutEffect, useId, useRef, useState, type ComponentProps } from "react";
import { CodeBlock, CodeBlockCopyButton, useIsCodeFenceIncomplete } from "streamdown";
import type { ThemePreset } from "../../styles/themePresets";
import styles from "./index.module.scss";

export const MarkdownThemeContext = createContext<ThemePreset | null>(null);

export function MarkdownCode({ node: _node, children, className, ...props }: ComponentProps<"code"> & { node?: unknown; "data-block"?: unknown }) {
  const incomplete = useIsCodeFenceIncomplete();
  const theme = useContext(MarkdownThemeContext)!;
  if (!("data-block" in props)) return <code {...props} className={className} data-streamdown="inline-code">{children}</code>;
  const language = className?.match(/language-([^\s]+)/)?.[1] ?? "";
  const code = typeof children === "string" ? children : isValidElement<{ children?: string }>(children) ? children.props.children ?? "" : "";
  return language === "mermaid"
    ? <MermaidDiagram source={code} theme={theme} incomplete={incomplete} />
    : <CodeBlock code={code} language={language} isIncomplete={incomplete} lineNumbers={false}><CodeBlockCopyButton aria-label="复制代码" /></CodeBlock>;
}

let mermaid_module: Promise<typeof import("@streamdown/mermaid")> | null = null;
let render_tail: Promise<unknown> = Promise.resolve();

/** Mermaid 共用可变配置，initialize 与 render 必须作为同一个串行步骤。 */
export function renderMermaid(source: string, theme: ThemePreset, id: string, active: () => boolean): Promise<string | null> {
  const task = render_tail.then(async () => {
    if (!active()) return null;
    mermaid_module ??= import("@streamdown/mermaid");
    const { mermaid } = await mermaid_module;
    if (!active()) return null;
    const color = theme.colors;
    const engine = mermaid.getMermaid({
      startOnLoad: false, securityLevel: "strict", suppressErrorRendering: true, theme: "base",
      secure: ["securityLevel", "startOnLoad", "suppressErrorRendering", "maxTextSize", "maxEdges"],
      themeVariables: {
        darkMode: theme.color_mode === "dark", background: color["--ez-surface"],
        primaryColor: color["--ez-primary-soft"], primaryTextColor: color["--ez-text"], primaryBorderColor: color["--ez-border-strong"],
        secondaryColor: color["--ez-surface-subtle"], secondaryTextColor: color["--ez-text"],
        tertiaryColor: color["--ez-surface-muted"], tertiaryTextColor: color["--ez-text"],
        lineColor: color["--ez-text-secondary"], textColor: color["--ez-text"],
        mainBkg: color["--ez-surface-subtle"], nodeBorder: color["--ez-border-strong"],
        edgeLabelBackground: color["--ez-surface"], fontFamily: "sans-serif",
      },
    });
    const { svg } = await engine.render(id, source);
    return active() ? svg : null;
  });
  // 一张图解析失败不能堵住后续图表；不执行失效且尚未开始的任务。
  render_tail = task.catch(() => undefined);
  return task;
}

function MermaidDiagram({ source, theme, incomplete }: Readonly<{ source: string; theme: ThemePreset; incomplete: boolean }>) {
  const id = `ez-mermaid-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const host = useRef<HTMLDivElement>(null);
  const [result, setResult] = useState<{ source: string; theme: ThemePreset; svg: string } | null>(null);
  useEffect(() => {
    let active = true;
    if (!incomplete) void renderMermaid(source, theme, id, () => active).then((svg) => {
      if (active && svg) setResult({ source, theme, svg });
    }).catch(() => { if (active) setResult(null); });
    return () => {
      active = false;
      // 替换期间保留原图占用空间，避免阅读位置跳动。
      if (host.current) host.current.style.minHeight = `${host.current.getBoundingClientRect().height}px`;
    };
  }, [source, theme, id, incomplete]);
  const svg = result?.source === source && result.theme === theme ? result.svg : null;
  useLayoutEffect(() => { if (svg && host.current) host.current.style.minHeight = ""; }, [svg]);
  return <div ref={host} className={styles.diagram} data-streamdown="mermaid-block">
    {svg ? <div dangerouslySetInnerHTML={{ __html: svg }} /> : <pre aria-label={incomplete ? "图表正在生成" : "图表源码"}>{source}</pre>}
  </div>;
}
