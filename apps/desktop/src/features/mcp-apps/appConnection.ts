import {
  AppBridge,
  type McpUiHostContext,
  type McpUiToolResultNotification,
} from '@modelcontextprotocol/ext-apps/app-bridge';
import type { McpUiResource, MessageCardSnapshot, SaveMessageCardSnapshotResult } from '@ez-assistant/protocol';
import { AppWindowTransport } from './windowTransport';
import type { AppLinkEndpoint } from './appLinks';
import type { AppToolBridge } from './appTools';

export type AppPageSource =
  | Readonly<{ kind: 'resource'; resource: McpUiResource; proxy_url: string }>
  | Readonly<{
      kind: 'external';
      url: string;
      approved_origin: string;
      host_origins: readonly string[];
    }>;

export type AppPageState = Readonly<{ status: 'connecting' | 'connected' | 'error'; message?: string }>;

export type CardSnapshotBridge = Readonly<{
  revision: number;
  snapshot: MessageCardSnapshot | null;
  schema: string | null;
  save: (revision: number, snapshot: MessageCardSnapshot) => Promise<SaveMessageCardSnapshotResult>;
  retire: () => void;
}>;

/** 页面生命周期 owner；不持有 Runtime 权限、登录凭据或 MCP client。 */
export class AppPageConnection {
  readonly frame = document.createElement('iframe');
  readonly #bridge: AppBridge;
  readonly #transport: AppWindowTransport;
  #closed = false;
  #loaded = false;
  #ready = false;
  #timeout: ReturnType<typeof setTimeout>;
  #initial_size_timer?: ReturnType<typeof setTimeout>;

  constructor(
    parent: HTMLElement,
    source: AppPageSource,
    context: McpUiHostContext,
    private readonly on_state: (state: AppPageState) => void,
    input?: Record<string, unknown>,
    result?: McpUiToolResultNotification['params'],
    private readonly card?: CardSnapshotBridge,
    private readonly links?: AppLinkEndpoint,
    private readonly tools?: AppToolBridge,
    initial_height?: number,
  ) {
    const url = new URL(source.kind === 'resource' ? source.proxy_url : source.url);
    let origin = 'null';
    if (source.kind === 'external') {
      if (
        !['http:', 'https:'].includes(url.protocol) ||
        url.username ||
        url.password ||
        url.origin !== source.approved_origin ||
        source.host_origins.includes(url.origin) ||
        url.origin === location.origin
      )
        throw new Error('应用来源未获授权');
      origin = url.origin;
    } else if (
      !['https:', 'http:'].includes(url.protocol) ||
      url.pathname !== '/ui/sandbox-proxy' ||
      url.search ||
      url.hash ||
      url.username ||
      url.password
    ) {
      throw new Error('应用沙箱地址无效');
    }
    this.frame.title = '应用界面';
    this.frame.sandbox.add('allow-scripts', 'allow-same-origin');
    this.frame.referrerPolicy = 'no-referrer';
    this.frame.allow = "camera 'none'; microphone 'none'; geolocation 'none'; clipboard-write 'none'";
    if (initial_height !== undefined) this.frame.style.height = `${initial_height}px`;
    // 带目标地址接入以避免 about:blank 的异步 load 被误判为首次业务加载。
    this.frame.src = url.href;
    parent.append(this.frame);
    if (!this.frame.contentWindow) throw new Error('无法创建应用页面');
    this.#transport = new AppWindowTransport(
      this.frame.contentWindow,
      origin,
      () => this.#fail('页面已导航或连接已失效，请重新加载'),
      source.kind === 'resource' && !!tools,
    );
    this.#bridge = new AppBridge(
      null,
      { name: 'EZ Assistant', version: __APP_VERSION__ },
      {
        ...(tools ? { serverTools: {} } : {}),
        ...(card || links ? {
            experimental: {
              'ez-assistant': {
                ...(card
                  ? { cardState: { version: 1 }, ...(card.schema ? { saveSnapshot: { version: 1 } } : {}) }
                  : {}),
                ...(links?.capabilities.includes('open-app') ? { openApp: { version: 1 } } : {}),
                ...(links?.capabilities.includes('post-message') ? { postMessage: { version: 1 } } : {}),
                ...(links?.capabilities.includes('set-composer-draft') ? { setComposerDraft: { version: 1 } } : {}),
              },
            },
          } : {}),
      },
      { hostContext: context },
    );
    if (tools) this.#bridge.oncalltool = (params, context) => {
      if (!this.#ready || this.#closed) throw new Error('应用未连接');
      return tools.call(params, context, this.#transport.consumeToolGesture(context.mcpReq.id));
    };
    this.#transport.onrequest = (request) => {
      if (
        links &&
        ['ez-assistant/open-app', 'ez-assistant/post-message', 'ez-assistant/set-composer-draft'].includes(
          request.method,
        )
      ) {
        void (async () => {
          try {
            if (!this.#ready || this.#closed || !request.params) throw new Error();
            const result = await links.request(request.method, request.params);
            if (!this.#closed)
              await this.#transport.send({ jsonrpc: '2.0', id: request.id, result: result as Record<string, unknown> });
          } catch {
            if (!this.#closed)
              await this.#transport.send({
                jsonrpc: '2.0',
                id: request.id,
                error: { code: -32000, message: '应用请求未完成，请核对入口、关联或用户确认' },
              });
          }
        })().catch(() => undefined);
        return true;
      }
      if (!card?.schema || request.method !== 'ez-assistant/save-snapshot') return false;
      void (async () => {
        try {
          const params = request.params;
          if (
            !this.#ready ||
            !params ||
            !Number.isSafeInteger(params.expected_revision) ||
            Number(params.expected_revision) < 0 ||
            params.schema !== card.schema ||
            !('snapshot' in params)
          )
            throw new Error();
          const saved = await card.save(Number(params.expected_revision), {
            schema: card.schema!,
            value: params.snapshot,
          });
          if (!this.#closed) await this.#transport.send({ jsonrpc: '2.0', id: request.id, result: saved });
        } catch {
          if (!this.#closed)
            await this.#transport.send({
              jsonrpc: '2.0',
              id: request.id,
              error: { code: -32000, message: '快照未保存，请读取当前状态后重试' },
            });
        }
      })().catch(() => undefined);
      return true;
    };
    this.#bridge.addEventListener('sandboxready', () => {
      if (this.#closed || source.kind !== 'resource') return;
      void this.#bridge
        .sendSandboxResourceReady({
          html: source.resource.html,
          csp: source.resource.ui?.csp ?? undefined,
          sandbox: 'allow-scripts',
        })
        .catch(() => this.#fail('应用资源加载失败'));
    });
    let settling_size = initial_height !== undefined;
    let initial_reported_height: string | undefined;
    this.#bridge.addEventListener('initialized', () => {
      if (this.#closed) return;
      this.#ready = true;
      clearTimeout(this.#timeout);
      this.on_state({ status: 'connected' });
      // 所有业务数据均在初始化完成后发送；只为已挂载卡片公布快照扩展。
      void (async () => {
        if (input) await this.#bridge.sendToolInput({ arguments: input });
        if (result) await this.#bridge.sendToolResult(result);
        if (card)
          await this.#transport.send({
            jsonrpc: '2.0',
            method: 'ez-assistant/notifications/card-state',
            params: { snapshot: card.snapshot, revision: card.revision, schema: card.schema },
          });
        if (this.#closed) return;
        links?.connect((method, params) => this.#transport.send({ jsonrpc: '2.0', method, params }));
        if (settling_size) {
          // 重新挂载时，插件可能先上报加载占位的尺寸。初始数据发出后合并这段时间的尺寸通知。
          this.#initial_size_timer = setTimeout(() => {
            this.#initial_size_timer = undefined;
            settling_size = false;
            if (initial_reported_height !== undefined) this.frame.style.height = initial_reported_height;
          }, 100);
        }
      })().catch(() => this.#fail('应用数据传递失败'));
    });
    this.#bridge.addEventListener('sizechange', ({ height }) => {
      if (this.#closed || typeof height !== 'number' || !Number.isFinite(height)) return;
      const bounded_height = `${Math.max(120, Math.min(height, 600))}px`;
      if (settling_size) initial_reported_height = bounded_height;
      else this.frame.style.height = bounded_height;
    });
    this.#bridge.onerror = () => this.#fail('应用通信失败');
    this.frame.addEventListener('load', this.#load);
    this.#timeout = setTimeout(() => this.#fail('页面未完成应用连接，可能不支持通信或限制嵌入'), 12_000);
    void this.#bridge.connect(this.#transport).catch(() => this.#fail('应用连接失败'));
    this.on_state({ status: 'connecting' });
  }

  updateContext(context: McpUiHostContext): void {
    if (!this.#closed) this.#bridge.setHostContext(context);
  }

  dispose(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.card?.retire();
    this.links?.retire();
    this.tools?.retire();
    clearTimeout(this.#timeout);
    clearTimeout(this.#initial_size_timer);
    this.frame.removeEventListener('load', this.#load);
    // 撤销本地端点不依赖页面答复；优雅 teardown 只给予通知机会，不能阻塞 owner 退出。
    if (this.#ready) void this.#bridge.teardownResource({}, { timeout: 300 }).catch(() => undefined);
    void this.#bridge.close();
    this.frame.remove();
  }

  #load = (): void => {
    if (this.frame.src === '') return;
    if (this.#loaded) this.#fail('页面已导航，请重新加载');
    else this.#loaded = true;
  };

  #fail(message: string): void {
    if (this.#closed) return;
    this.dispose();
    this.on_state({ status: 'error', message });
  }
}
