import {
  Component,
  createContext,
  type ComponentProps,
  type ErrorInfo,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
  useContext,
  useEffect,
  useMemo,
  useState,
} from "react";
import {
  Streamdown,
  defaultRehypePlugins,
  type Components,
  type PluginConfig,
  type UrlTransform,
} from "streamdown";
import "streamdown/styles.css";
import { openExternalHttpUrl } from "../../native-bridge/openExternalUrl";
import styles from "./index.module.scss";

type MarkdownContentProps = Readonly<{
  text: string;
  is_streaming?: boolean;
  allow_relative_local_resources?: boolean;
  load_local_image?: (reference: string) => Promise<string>;
  on_local_resource_context_menu?: (
    reference: string,
    location: Readonly<{ x: number; y: number }>,
  ) => void;
  on_local_resource_open?: (reference: string) => void;
}>;

type LocalResourceContextValue = Readonly<{
  load_image?: (reference: string) => Promise<string>;
  menu?: (reference: string, location: Readonly<{ x: number; y: number }>) => void;
  open?: (reference: string) => void;
}>;

const LOCAL_RESOURCE_PREFIX = "https://local-resource.invalid/";
const LocalResourceContext = createContext<LocalResourceContextValue>({});
const markdownComponents = { a: SafeLink, img: SafeImage } as Components;

export function MarkdownContent({
  text,
  is_streaming = false,
  allow_relative_local_resources = false,
  load_local_image,
  on_local_resource_context_menu,
  on_local_resource_open,
}: MarkdownContentProps) {
  const plugins = useMarkdownPlugins(text);
  const local_resources = useMemo<LocalResourceContextValue>(() => ({
    load_image: load_local_image,
    menu: on_local_resource_context_menu,
    open: on_local_resource_open,
  }), [load_local_image, on_local_resource_context_menu, on_local_resource_open]);
  const rehype_plugins = useMemo<NonNullable<ComponentProps<typeof Streamdown>["rehypePlugins"]>>(() => {
    const local_plugin: [typeof localResourceRewritePlugin, { allow_relative: boolean }] = [
      localResourceRewritePlugin,
      { allow_relative: allow_relative_local_resources },
    ];
    return [local_plugin, ...Object.values(defaultRehypePlugins)];
  }, [allow_relative_local_resources]);
  const url_transform = useMemo<UrlTransform>(() => (url) => {
    if (isSafeHttpUrl(url)) return url;
    return isLocalResourceReference(url, allow_relative_local_resources)
      ? `${LOCAL_RESOURCE_PREFIX}${encodeURIComponent(url)}`
      : null;
  }, [allow_relative_local_resources]);
  return (
    <MarkdownBoundary fallback={text}>
      <LocalResourceContext.Provider value={local_resources}>
        <Streamdown
          key={allow_relative_local_resources ? "local-relative" : "local-explicit"}
          className={styles.markdown}
          components={markdownComponents}
          controls={{ code: { copy: true, download: false }, mermaid: false, table: false }}
          isAnimating={false}
          linkSafety={{ enabled: false }}
          lineNumbers={false}
          mode={is_streaming ? "streaming" : "static"}
          parseIncompleteMarkdown={is_streaming}
          plugins={plugins}
          rehypePlugins={rehype_plugins}
          skipHtml
          urlTransform={url_transform}
        >
          {text}
        </Streamdown>
      </LocalResourceContext.Provider>
    </MarkdownBoundary>
  );
}

function SafeLink({ href, children, ...props }: ComponentProps<"a">) {
  const local = useContext(LocalResourceContext);
  const safe_href = typeof href === "string" && isSafeHttpUrl(href) ? href : null;
  const local_reference = typeof href === "string" ? decodeLocalResourceReference(href) : null;
  if (local_reference && local.open) {
    return (
      <button
        className={styles.local_link}
        onClick={() => local.open?.(local_reference)}
        onContextMenu={(event) => openLocalMenuFromPointer(event, local, local_reference)}
        onKeyDown={(event) => openLocalMenuFromKeyboard(event, local, local_reference)}
        type="button"
      >
        <span aria-hidden="true">↗</span>{children}
      </button>
    );
  }
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

function SafeImage({ src, alt }: ComponentProps<"img">) {
  const local = useContext(LocalResourceContext);
  const local_reference = typeof src === "string" ? decodeLocalResourceReference(src) : null;
  if (local_reference && local.load_image) {
    return <LocalImage alt={alt ?? ""} load={local.load_image} onOpen={local.open} reference={local_reference} />;
  }
  return typeof src === "string" && isSafeHttpUrl(src)
    ? <img alt={alt ?? ""} src={src} />
    : <span className={styles.image_unavailable}>{alt || "图片不可用"}</span>;
}

function LocalImage(props: Readonly<{
  alt: string;
  load: (reference: string) => Promise<string>;
  onOpen?: (reference: string) => void;
  reference: string;
}>) {
  const local = useContext(LocalResourceContext);
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let active = true;
    let object_url: string | null = null;
    setFailed(false);
    setUrl(null);
    void props.load(props.reference).then((value) => {
      object_url = value;
      if (active) setUrl(value);
      else URL.revokeObjectURL(value);
    }).catch(() => {
      if (active) setFailed(true);
    });
    return () => {
      active = false;
      if (object_url) URL.revokeObjectURL(object_url);
    };
  }, [props.load, props.reference]);
  if (failed) return <span className={styles.image_unavailable}>{props.alt || "图片加载失败"}</span>;
  if (!url) return <span className={styles.image_unavailable}>{props.alt || "正在加载图片…"}</span>;
  return props.onOpen ? (
    <button
      className={styles.local_image}
      onClick={() => props.onOpen?.(props.reference)}
      onContextMenu={(event) => openLocalMenuFromPointer(event, local, props.reference)}
      onKeyDown={(event) => openLocalMenuFromKeyboard(event, local, props.reference)}
      type="button"
    >
      <img alt={props.alt} src={url} />
    </button>
  ) : <img alt={props.alt} src={url} />;
}

function openLocalMenuFromPointer(
  event: MouseEvent<HTMLElement>,
  local: LocalResourceContextValue,
  reference: string,
) {
  if (!local.menu) return;
  event.preventDefault();
  event.stopPropagation();
  event.currentTarget.focus();
  local.menu(reference, { x: event.clientX, y: event.clientY });
}

function openLocalMenuFromKeyboard(
  event: KeyboardEvent<HTMLElement>,
  local: LocalResourceContextValue,
  reference: string,
) {
  if (!local.menu || (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10"))) return;
  event.preventDefault();
  event.stopPropagation();
  const bounds = event.currentTarget.getBoundingClientRect();
  local.menu(reference, { x: bounds.left + 12, y: bounds.bottom });
}

function useMarkdownPlugins(text: string): PluginConfig {
  const [plugins, setPlugins] = useState<PluginConfig>({});
  const needs_code = /```[a-zA-Z0-9_-]*\n/.test(text);
  const needs_math = /(^|[^\\])\$\$?[\s\S]*?\$\$?/.test(text);
  const needs_mermaid = /```mermaid\b/.test(text);

  useEffect(() => {
    let active = true;
    const tasks: Promise<void>[] = [];
    if (needs_code) {
      tasks.push(loadCodePlugin().then((code) => {
        if (active) {
          setPlugins((current) => ({ ...current, code }));
        }
      }).catch(() => undefined));
    }
    if (needs_math) {
      tasks.push(loadMathPlugin().then((math) => {
        if (active) {
          setPlugins((current) => ({ ...current, math }));
        }
      }).catch(() => undefined));
    }
    if (needs_mermaid) {
      tasks.push(loadMermaidPlugin().then((mermaid) => {
        if (active) {
          setPlugins((current) => ({ ...current, mermaid }));
        }
      }).catch(() => undefined));
    }
    void Promise.all(tasks);
    return () => {
      active = false;
    };
  }, [needs_code, needs_math, needs_mermaid]);

  return plugins;
}

let code_plugin_promise: Promise<NonNullable<PluginConfig["code"]>> | null = null;
let math_plugin_promise: Promise<NonNullable<PluginConfig["math"]>> | null = null;
let mermaid_plugin_promise: Promise<NonNullable<PluginConfig["mermaid"]>> | null = null;

function loadCodePlugin(): Promise<NonNullable<PluginConfig["code"]>> {
  code_plugin_promise ??= import("@streamdown/code").then(({ code }) => code);
  return code_plugin_promise;
}

function loadMathPlugin(): Promise<NonNullable<PluginConfig["math"]>> {
  math_plugin_promise ??= import("@streamdown/math").then(({ math }) => math);
  return math_plugin_promise;
}

function loadMermaidPlugin(): Promise<NonNullable<PluginConfig["mermaid"]>> {
  mermaid_plugin_promise ??= import("@streamdown/mermaid").then(({ mermaid }) => mermaid);
  return mermaid_plugin_promise;
}

function isSafeHttpUrl(url: string): boolean {
  try {
    const parsed = new URL(url);
    return (parsed.protocol === "http:" || parsed.protocol === "https:") && Boolean(parsed.hostname);
  } catch {
    return false;
  }
}

function isLocalResourceReference(url: string, allow_relative: boolean): boolean {
  try {
    const parsed = new URL(url);
    return parsed.protocol === "file:"
      && !parsed.username
      && !parsed.password
      && !parsed.port
      && (!parsed.hostname || parsed.hostname === "localhost")
      && Boolean(parsed.pathname)
      && !parsed.search
      && !parsed.hash;
  } catch {
    return allow_relative
      && !url.startsWith("#")
      && !url.startsWith("/")
      && !url.startsWith("//")
      && !url.includes(":");
  }
}

type MarkdownNode = Readonly<{
  type?: string;
  tagName?: string;
  properties?: Record<string, unknown>;
  children?: MarkdownNode[];
}>;

function localResourceRewritePlugin(options: Readonly<{ allow_relative: boolean }>) {
  return (tree: MarkdownNode) => {
    visitMarkdownNodes(tree, (node) => {
      if (node.type !== "element" || !node.properties) return;
      const property = node.tagName === "a" ? "href" : node.tagName === "img" ? "src" : null;
      if (!property) return;
      const value = node.properties[property];
      if (typeof value === "string" && isLocalResourceReference(value, options.allow_relative)) {
        node.properties[property] = `${LOCAL_RESOURCE_PREFIX}${encodeURIComponent(value)}`;
      }
    });
  };
}

function visitMarkdownNodes(node: MarkdownNode, visit: (node: MarkdownNode) => void): void {
  visit(node);
  node.children?.forEach((child) => visitMarkdownNodes(child, visit));
}

function decodeLocalResourceReference(value: string): string | null {
  if (!value.startsWith(LOCAL_RESOURCE_PREFIX)) return null;
  try {
    return decodeURIComponent(value.slice(LOCAL_RESOURCE_PREFIX.length));
  } catch {
    return null;
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
