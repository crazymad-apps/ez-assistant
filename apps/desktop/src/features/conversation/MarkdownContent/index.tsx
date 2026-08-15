import { Component, type ComponentProps, type ErrorInfo, type ReactNode, useEffect, useState } from "react";
import { Streamdown, type Components, type PluginConfig, type UrlTransform } from "streamdown";
import "streamdown/styles.css";
import { openExternalHttpUrl } from "../../../native-bridge/openExternalUrl";
import styles from "./index.module.scss";

type MarkdownContentProps = Readonly<{
  text: string;
  is_streaming?: boolean;
}>;

const safeUrlTransform: UrlTransform = (url) => isSafeHttpUrl(url) ? url : null;
const markdownComponents = { a: SafeLink } as Components;

export function MarkdownContent({ text, is_streaming = false }: MarkdownContentProps) {
  const plugins = useMarkdownPlugins(text);
  return (
    <MarkdownBoundary fallback={text}>
      <Streamdown
        className={styles.markdown}
        components={markdownComponents}
        controls={{ code: { copy: true, download: false }, mermaid: false, table: false }}
        isAnimating={false}
        lineNumbers={false}
        mode={is_streaming ? "streaming" : "static"}
        parseIncompleteMarkdown={is_streaming}
        plugins={plugins}
        skipHtml
        urlTransform={safeUrlTransform}
      >
        {text}
      </Streamdown>
    </MarkdownBoundary>
  );
}

function SafeLink({ href, children, ...props }: ComponentProps<"a">) {
  const safe_href = typeof href === "string" && isSafeHttpUrl(href) ? href : null;
  return safe_href ? (
    <a
      {...props}
      href={safe_href}
      onClick={(event) => {
        event.preventDefault();
        void openExternalHttpUrl(safe_href);
      }}
      rel="noreferrer"
    >
      {children}
    </a>
  ) : <span>{children}</span>;
}

function useMarkdownPlugins(text: string): PluginConfig {
  const [plugins, setPlugins] = useState<PluginConfig>({});

  useEffect(() => {
    let active = true;
    const tasks: Promise<void>[] = [];
    if (/```[a-zA-Z0-9_-]*\n/.test(text)) {
      tasks.push(import("@streamdown/code").then(({ code }) => {
        if (active) {
          setPlugins((current) => ({ ...current, code }));
        }
      }).catch(() => undefined));
    }
    if (/(^|[^\\])\$\$?[\s\S]*?\$\$?/.test(text)) {
      tasks.push(import("@streamdown/math").then(({ math }) => {
        if (active) {
          setPlugins((current) => ({ ...current, math }));
        }
      }).catch(() => undefined));
    }
    if (/```mermaid\b/.test(text)) {
      tasks.push(import("@streamdown/mermaid").then(({ mermaid }) => {
        if (active) {
          setPlugins((current) => ({ ...current, mermaid }));
        }
      }).catch(() => undefined));
    }
    void Promise.all(tasks);
    return () => {
      active = false;
    };
  }, [text]);

  return plugins;
}

function isSafeHttpUrl(url: string): boolean {
  try {
    const parsed = new URL(url);
    return (parsed.protocol === "http:" || parsed.protocol === "https:") && Boolean(parsed.hostname);
  } catch {
    return false;
  }
}

class MarkdownBoundary extends Component<
  Readonly<{ fallback: string; children: ReactNode }>,
  Readonly<{ failed: boolean }>
> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(_error: Error, _info: ErrorInfo): void {
    // The plain-text fallback is intentionally silent and keeps the message usable.
  }

  componentDidUpdate(previous: Readonly<{ fallback: string }>): void {
    if (this.state.failed && previous.fallback !== this.props.fallback) {
      this.setState({ failed: false });
    }
  }

  render() {
    return this.state.failed
      ? <pre className={styles.fallback}>{this.props.fallback}</pre>
      : this.props.children;
  }
}
