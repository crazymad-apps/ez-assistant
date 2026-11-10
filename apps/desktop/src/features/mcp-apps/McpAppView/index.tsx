import { useEffect, useRef, useState } from 'react';
import type { McpUiHostContext, McpUiToolResultNotification } from '@modelcontextprotocol/ext-apps/app-bridge';
import { AppPageConnection, type AppPageSource, type AppPageState, type CardSnapshotBridge } from '../appConnection';
import styles from './index.module.scss';
import type { AppLinkEndpoint } from '../appLinks';
import { AppToolBridge, type AppToolPage, type AppToolPrompt } from '../appTools';
import { AppToolApprovalDialog } from '../AppToolApprovalDialog';

export type McpAppViewProps = Readonly<{
  source: AppPageSource;
  initial_height?: number;
  context: McpUiHostContext;
  input?: Record<string, unknown>;
  result?: McpUiToolResultNotification['params'];
  card?: CardSnapshotBridge;
  links?: AppLinkEndpoint;
  tool_page?: AppToolPage;
  show_tool_approval?: boolean;
  tool_page_current?: () => boolean;
  on_state_change?: (state: AppPageState) => void;
}>;

function withPageTheme(context: McpUiHostContext): McpUiHostContext {
  return {
    ...context,
    theme: getComputedStyle(document.documentElement).colorScheme.includes('dark') ? 'dark' : 'light',
  };
}

/** 容器卸载即撤销连接；调用方必须在用户域、插件或入口身份变化时卸载/替换 source。 */
export function McpAppView(props: McpAppViewProps) {
  const parent = useRef<HTMLDivElement>(null);
  const connection = useRef<AppPageConnection | null>(null);
  const [state, setState] = useState<AppPageState>({ status: 'connecting' });
  const [prompts, setPrompts] = useState<ReadonlyMap<string, AppToolPrompt>>(new Map());
  const latest = useRef(props);
  latest.current = props;

  useEffect(() => {
    const surface = parent.current;
    if (!surface) return;
    const current = latest.current;
    let mounted_page = true;
    let mounted: AppPageConnection | undefined;
    setPrompts(new Map());
    // React 开发模式会同步重放首次副作用。等清理完成后再创建 iframe，避免试挂载撤销共享句柄。
    queueMicrotask(() => {
      if (!mounted_page) return;
      const tools = current.tool_page && new AppToolBridge(
        current.tool_page,
        () => current.tool_page_current?.() ?? true,
        (request_id, prompt) => mounted_page && setPrompts((previous) => {
          const next = new Map(previous);
          if (prompt) next.set(request_id, prompt);
          else next.delete(request_id);
          return next;
        }),
      );
      try {
        mounted = new AppPageConnection(
          surface,
          props.source,
          withPageTheme(latest.current.context),
          (state) => {
            setState(state);
            current.on_state_change?.(state);
          },
          current.input,
          current.result,
          current.card,
          current.links,
          tools,
          current.initial_height,
        );
        connection.current = mounted;
      } catch (error) {
        tools?.retire();
        setState({ status: 'error', message: error instanceof Error ? error.message : '应用加载失败' });
      }
    });
    return () => {
      mounted_page = false;
      connection.current = null;
      mounted?.dispose();
    };
  }, [props.source, props.tool_page]);

  useEffect(() => {
    const update = () => connection.current?.updateContext(withPageTheme(props.context));
    update();
    const observer = new MutationObserver(update);
    observer.observe(document.documentElement, { attributes: true });
    return () => observer.disconnect();
  }, [props.context, props.source]);

  const prompt = prompts.values().next().value;
  return (
    <section className={styles.page} data-app-state={state.status} aria-label="应用页面">
      {state.status === 'connecting' && (
        <p className={props.initial_height !== undefined ? styles.loading_overlay : undefined} role="status">
          正在连接应用…
        </p>
      )}
      {state.status === 'error' && <p role="alert">{state.message}</p>}
      {prompt && <p role="status">工具调用等待确认</p>}
      <div
        ref={parent}
        className={styles.surface}
        style={{ minHeight: state.status === 'connecting' ? props.initial_height : undefined }}
      />
      {prompt && props.show_tool_approval && <AppToolApprovalDialog key={prompt.approval.approval_id} prompt={prompt} />}
    </section>
  );
}
