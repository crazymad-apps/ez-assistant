import type { PluginUiCapability, UiAppTarget } from '@ez-assistant/protocol';

export type AppTarget = UiAppTarget | Readonly<{ type: 'external'; url: string }>;
export type AppOpenRequest = Readonly<{ target: AppTarget; arguments: Record<string, unknown>; instance_key?: string }>;
type Notify = (method: string, params: Record<string, unknown>) => Promise<void>;

/** 页面之间的短期双向关联；只有实际参与的两个端点拥有句柄，关闭后不排队重放。 */
export class AppLinkEndpoint {
  readonly #links = new Map<string, AppLinkEndpoint>();
  #notify: Notify | null = null;
  #live = true;
  readonly #lifetime = new AbortController();
  readonly #waiters = new Set<(ready: boolean) => void>();
  constructor(
    readonly capabilities: readonly PluginUiCapability[],
    private readonly open: (request: AppOpenRequest) => Promise<AppLinkEndpoint>,
    private readonly draft: (text: string, active: () => boolean, signal: AbortSignal) => Promise<void>,
    private readonly current: () => boolean = () => true,
  ) {}
  get live(): boolean {
    return this.#live && this.current();
  }
  connect(notify: Notify): void {
    if (!this.live || this.#notify) throw new Error('页面连接已失效');
    this.#notify = notify;
    for (const done of this.#waiters) done(true);
    this.#waiters.clear();
  }
  async ready(): Promise<void> {
    if (!this.live) throw new Error('页面已关闭');
    if (this.#notify) return;
    await new Promise<void>((resolve, reject) => {
      const done = (ready: boolean) => {
        clearTimeout(timer);
        this.#waiters.delete(done);
        if (ready) resolve();
        else reject(new Error('应用未连接'));
      };
      const timer = setTimeout(() => done(false), 15_000);
      this.#waiters.add(done);
    });
  }
  async request(method: string, params: Record<string, unknown>): Promise<unknown> {
    if (!this.live || !this.#notify) throw new Error('页面未连接');
    if (method === 'ez-assistant/open-app' && this.capabilities.includes('open-app')) {
      if (
        !record(params.target) ||
        !record(params.arguments ?? {}) ||
        (params.instance_key !== undefined &&
          (typeof params.instance_key !== 'string' || params.instance_key.length > 64))
      )
        throw new Error('应用参数无效');
      const peer = await this.open({
        target: params.target as AppTarget,
        arguments: (params.arguments ?? {}) as Record<string, unknown>,
        instance_key: params.instance_key as string | undefined,
      });
      await peer.ready();
      if (!this.live || !peer.live || peer === this || this.#links.size >= 32 || peer.#links.size >= 32)
        throw new Error('应用关联不可用');
      const handle = crypto.randomUUID();
      this.#links.set(handle, peer);
      peer.#links.set(handle, this);
      try {
        await peer.#notify!('ez-assistant/notifications/link-opened', { handle, arguments: params.arguments ?? {} });
      } catch {
        this.#links.delete(handle);
        peer.#links.delete(handle);
        throw new Error('应用关联失败');
      }
      if (!this.live || !peer.live) throw new Error('应用关联已关闭');
      return { handle };
    }
    if (method === 'ez-assistant/post-message' && this.capabilities.includes('post-message')) {
      const peer = typeof params.handle === 'string' ? this.#links.get(params.handle) : undefined;
      if (
        !peer ||
        !peer.live ||
        !peer.#notify ||
        !('payload' in params) ||
        new TextEncoder().encode(JSON.stringify(params.payload)).length > 128 * 1024
      )
        throw new Error('应用关联已关闭或消息过大');
      await peer.#notify('ez-assistant/notifications/link-message', { handle: params.handle, payload: params.payload });
      return {};
    }
    if (method === 'ez-assistant/set-composer-draft' && this.capabilities.includes('set-composer-draft')) {
      if (
        typeof params.text !== 'string' ||
        !params.text.trim() ||
        new TextEncoder().encode(params.text).length > 64 * 1024
      )
        throw new Error('草稿无效或过长');
      await this.draft(params.text, () => this.live, this.#lifetime.signal);
      if (!this.live) throw new Error('页面已关闭');
      return { filled: true };
    }
    throw new Error('页面能力未授权');
  }
  retire(): void {
    if (!this.#live) return;
    this.#live = false;
    this.#lifetime.abort();
    this.#notify = null;
    for (const done of this.#waiters) done(false);
    this.#waiters.clear();
    for (const [handle, peer] of this.#links) {
      peer.#links.delete(handle);
      void peer.#notify?.('ez-assistant/notifications/link-closed', { handle }).catch(() => undefined);
    }
    this.#links.clear();
  }
}
function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

export function normalizeAppTarget(target: AppTarget, host_origins: readonly string[]): AppTarget {
  if (target.type === 'external') {
    const url = new URL(target.url);
    if (
      target.url.length > 16384 ||
      !['http:', 'https:'].includes(url.protocol) ||
      url.username ||
      url.password ||
      host_origins.includes(url.origin)
    )
      throw new Error('请输入与宿主不同源且不含用户名或密码的 HTTP 或 HTTPS 地址。');
    return { type: 'external', url: url.href };
  }
  if (target.type === 'plugin' && typeof target.name === 'string' && typeof target.entry_id === 'string')
    return { type: 'plugin', name: target.name, entry_id: target.entry_id };
  if (
    target.type === 'mcp' &&
    target.target &&
    typeof target.resource_uri === 'string' &&
    typeof target.entry_id === 'string'
  )
    return { type: 'mcp', target: target.target, resource_uri: target.resource_uri, entry_id: target.entry_id };
  throw new Error('应用入口无效');
}
