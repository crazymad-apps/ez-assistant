import type { UiToolAction, UiToolApproval } from '@ez-assistant/protocol';
import { McpUiToolResultNotificationSchema, type AppBridge } from '@modelcontextprotocol/ext-apps/app-bridge';
import type { RuntimeClient } from '../../runtime-client/RuntimeClient';

export type AppToolPage = Readonly<{ client: RuntimeClient; instance_id: string }>;
export type AppToolPrompt = Readonly<{
  approval: UiToolApproval;
  decide: (allow: boolean) => Promise<void>;
}>;
type AppToolCall = NonNullable<AppBridge['oncalltool']>;

function command(page: AppToolPage, action: UiToolAction, signal?: AbortSignal) {
  return page.client.command({ type: 'ui_tool', payload: { instance_id: page.instance_id, action } }, { signal });
}

export function releaseAppToolPage(page: AppToolPage): void {
  void command(page, { type: 'release' }).catch(() => undefined);
}

const reasons: Record<string, string> = {
  ui_tool_permission_denied: '工具调用未获授权。',
  ui_tool_approval_timeout: '工具授权已超时。',
  ui_tool_approval_unavailable: '无法安全展示此次工具调用的参数。',
  ui_tool_source_retired: '工具来源已失效，请重新连接应用。',
  ui_tool_request_limit: '应用调用过多，请等候或重新连接。',
  mcp_invalid_arguments: '工具参数不符合声明。',
  mcp_catalog_changed: '此工具对当前页面不可用。',
};

/** 只转发当前 iframe 的标准工具调用。所有授权和实际请求状态由 Runtime 持有。 */
export class AppToolBridge {
  readonly #lifetime = new AbortController();

  constructor(
    private readonly page: AppToolPage,
    private readonly current: () => boolean,
    private readonly prompt: (request_id: string, prompt: AppToolPrompt | null) => void,
  ) {}

  async call(
    params: Parameters<AppToolCall>[0],
    context: Parameters<AppToolCall>[1],
    user_gesture: boolean,
  ): ReturnType<AppToolCall> {
    const id = context.mcpReq.id;
    const request_id = `${typeof id}:${String(id)}`;
    const signal = AbortSignal.any([context.mcpReq.signal, this.#lifetime.signal]);
    let terminal = false;
    let previous_approval: string | null = null;
    try {
      this.ensureCurrent(signal);
      let state = (await command(
        this.page, {
          type: 'start', request_id, name: params.name, arguments: params.arguments ?? {},
          ...(user_gesture ? { user_gesture } : {}),
        }, signal,
      )).payload;
      while (true) {
        this.ensureCurrent(signal);
        if (state.type === 'failed') {
          terminal = true;
          return {
            isError: true,
            content: [{
              type: 'text',
              text: state.remote_may_have_executed
                ? '工具执行结果未知，请在应用中核对结果。'
                : reasons[state.reason] ?? '工具调用未完成。',
            }],
          };
        }
        if (state.type === 'complete') {
          terminal = true;
          return McpUiToolResultNotificationSchema.parse({
            method: 'ui/notifications/tool-result',
            params: state.result,
          }).params;
        }
        const approval = state.type === 'approval' ? state.approval : null;
        const approval_id = approval?.approval_id ?? null;
        if (approval_id !== previous_approval) {
          previous_approval = approval_id;
          this.prompt(request_id, approval ? {
            approval,
            decide: async (allow) => {
              this.ensureCurrent(signal);
              await command(this.page, {
                type: 'decide', request_id, approval_id: approval.approval_id,
                decision: allow ? 'allow_once' : 'deny',
              }, signal);
            },
          } : null);
        }
        await waitForPoll(signal);
        this.ensureCurrent(signal);
        state = (await command(this.page, { type: 'poll', request_id }, signal)).payload;
      }
    } catch {
      throw new Error('应用工具连接已结束，执行结果可能未知。');
    } finally {
      if (previous_approval) this.prompt(request_id, null);
      if (!terminal && !this.#lifetime.signal.aborted)
        void command(this.page, { type: 'cancel', request_id }).catch(() => undefined);
    }
  }

  retire(): void {
    if (this.#lifetime.signal.aborted) return;
    this.#lifetime.abort();
    releaseAppToolPage(this.page);
  }

  private ensureCurrent(signal: AbortSignal): void {
    if (signal.aborted || !this.current()) throw new Error('页面已失效');
  }
}

function waitForPoll(signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const cancel = () => {
      clearTimeout(timer);
      reject(new Error('调用已取消'));
    };
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', cancel);
      resolve();
    }, 500);
    signal.addEventListener('abort', cancel, { once: true });
  });
}
