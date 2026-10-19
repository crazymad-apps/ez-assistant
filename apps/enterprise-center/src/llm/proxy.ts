import { once } from 'node:events';
import type { IncomingHttpHeaders } from 'node:http';
import type { FastifyInstance, FastifyReply, FastifyRequest } from 'fastify';
import type { IdentityService } from '../identity/service.js';
import { IdentityError } from '../identity/errors.js';
import { requestCredential } from '../security.js';
import type { ModelsService } from '../models/service.js';
import type { ModelSelection, Protocol } from '../contracts/models.js';
import type { IdentityUser } from '../contracts/identity.js';
import { uuid } from '../models/parameters.js';
import type { ModelOperation } from './body.js';
import { RequestBodies, requestModel } from './body.js';
import { CallRecords } from './records.js';
import type { CallRecording } from './records.js';
import { ResponseObserver } from './response-observer.js';
import { UsageObserver } from './usage.js';
import { ControlError } from './errors.js';
import { transfer, responseHeaders, requestHeaders, TransferError } from './transport.js';
import type { TransferSink, ProxyRequestHeaders } from './transport.js';
import { ResponsesWebsocket, WebsocketResponseError } from './websocket.js';
import type { WebsocketLimits } from './websocket.js';

export type ProxyOptions = { connectTimeout?: number; idleTimeout?: number; websocket?: WebsocketLimits };

function control(reply: FastifyReply, requestId: string, error: unknown): void {
  if (error instanceof WebsocketResponseError) {
    void reply.header('cache-control', 'no-store').status(error.status).send(error.body);
    return;
  }
  const failure = error instanceof ControlError ? error : new ControlError('llm_proxy_unavailable');
  void reply
    .header('x-ez-center-control', '1')
    .header('cache-control', 'no-store')
    .header('x-content-type-options', 'nosniff')
    .status(failure.status)
    .send({
      error: {
        code: failure.code,
        message: failure.message,
        request_id: requestId,
        ...(failure.code === 'previous_response_not_found'
          ? { type: 'invalid_request_error', param: 'previous_response_id' }
          : {}),
      },
    });
}

/** 服务 owner 跟踪从准入到文件结算的整个 Promise，关闭先取消网络再等待记录。 */
export class ModelProxy {
  private readonly shutdown = new AbortController();
  private readonly pending = new Set<Promise<unknown>>();
  private readonly websocket: ResponsesWebsocket;
  private readonly unsubscribe: (() => void)[];
  private readonly bridgeRequests = new Set<{ user: number; provider: string; abort: AbortController }>();

  constructor(
    private readonly identities: IdentityService,
    private readonly models: ModelsService,
    readonly records: CallRecords,
    private readonly options: ProxyOptions,
  ) {
    this.websocket = new ResponsesWebsocket(options.websocket);
    this.unsubscribe = [
      identities.onRevoked((user) => {
        for (const request of this.bridgeRequests)
          if (user === null || request.user === user)
            request.abort.abort(new DOMException('identity_revoked', 'AbortError'));
        this.websocket.revokeUser(user);
      }),
      models.onHistoryInvalidated((provider) => {
        for (const request of this.bridgeRequests)
          if (request.provider === provider)
            request.abort.abort(new DOMException('bridge_configuration_changed', 'AbortError'));
        this.websocket.revokeProvider(provider);
      }),
    ];
  }

  run<T>(work: () => Promise<T>): Promise<T> {
    if (this.shutdown.signal.aborted) return Promise.reject(new ControlError('llm_proxy_unavailable'));
    const task = work();
    this.pending.add(task);
    void task.then(
      () => this.pending.delete(task),
      () => this.pending.delete(task),
    );
    return task;
  }

  async onModuleDestroy(): Promise<void> {
    await this.close();
  }

  async close(): Promise<void> {
    this.shutdown.abort(new DOMException('center_shutdown', 'AbortError'));
    for (const unsubscribe of this.unsubscribe.splice(0)) unsubscribe();
    this.websocket.close();
    let timer: NodeJS.Timeout | undefined;
    const done = Promise.allSettled([...this.pending, this.records.close(), this.models.onModuleDestroy()]);
    const timeout = new Promise<void>((resolve) => {
      timer = setTimeout(() => {
        process.stderr.write('CENTER_LLM_SHUTDOWN_TIMEOUT\n');
        resolve();
      }, 10000);
    });
    await Promise.race([done, timeout]);
    clearTimeout(timer);
  }

  async execute(input: {
    key: string;
    user: IdentityUser;
    selection: ModelSelection;
    protocol: Protocol;
    body: Buffer;
    signal: AbortSignal;
    sink: TransferSink;
    kind: 'proxy' | 'admin_test';
    operation?: ModelOperation;
    historyBinding?: string;
    requestHeaders?: ProxyRequestHeaders;
    finish?: (signal: AbortSignal) => Promise<void>;
    admitted?: (record: CallRecording) => void;
    sent?: () => void;
  }) {
    return this.run(async () => {
      const signal = AbortSignal.any([input.signal, this.shutdown.signal]);
      signal.throwIfAborted();
      const recording = await this.records.begin({
        user_id: input.user.id,
        username: input.user.username,
        provider_instance_id: input.selection.provider_instance_id,
        model_id: input.selection.model_id,
        kind:
          input.operation === 'compact'
            ? 'context_compaction'
            : input.operation === 'input_tokens'
              ? 'input_tokens'
              : input.kind,
        protocol: input.protocol,
      });
      input.admitted?.(recording);
      recording.request?.append(input.body);
      let status: number | null = null;
      let upstream = false;
      let transferSignal = signal;
      const bridgeRequest = {
        user: input.user.id,
        provider: input.selection.provider_instance_id,
        abort: new AbortController(),
      };
      try {
        const parsed = requestModel(input.body, input.operation);
        if (parsed.model !== input.selection.model_id) throw new ControlError('llm_request_invalid');
        // 先登记准入等待期，覆盖异步准入结束到实际建连间的配置/主体撤销。
        // 只有最终走 WS 的请求使用此取消信号，不改变普通 HTTP 的生命周期。
        this.bridgeRequests.add(bridgeRequest);
        const route = await this.models.admit(input.key, input.selection, input.protocol, input.kind === 'admin_test', {
          request: parsed,
          binding: input.historyBinding,
          operation: input.operation,
        });
        signal.throwIfAborted();
        recording.row.provider_name = route.provider.connection.display_name;
        if (input.operation !== 'input_tokens')
          recording.usage = new UsageObserver(input.protocol, route.provider.connection.provider_type);
        recording.observation = new ResponseObserver(recording.row, recording.usage);
        signal.throwIfAborted();
        const bridge =
          route.provider.connection.responses_transport === 'websocket' &&
          input.protocol === 'open_ai_responses' &&
          input.operation !== 'input_tokens' &&
          input.kind === 'proxy';
        recording.row.response_source = bridge
          ? input.operation === 'compact'
            ? 'websocket_compact'
            : 'websocket_bridge'
          : 'upstream_http';
        if (!bridge) this.bridgeRequests.delete(bridgeRequest);
        transferSignal = bridge ? AbortSignal.any([signal, bridgeRequest.abort.signal]) : signal;
        transferSignal.throwIfAborted();
        upstream = !bridge;
        // 仅已准入的业务接续转换协议；HTTP 与辅助调用继续保留原始 Buffer。
        const transferInput = {
          endpoint: route.provider.connection.endpoint,
          protocol: input.protocol,
          operation: input.operation,
          apiKey: route.provider.api_key,
          requestHeaders: input.requestHeaders,
          body: input.body,
          signal: transferSignal,
          connectTimeout: this.options.connectTimeout ?? 10000,
          idleTimeout: this.options.idleTimeout ?? 300000,

          sent: () => {
            input.body = Buffer.alloc(0);
            input.sent?.();
          },

          sink: {
            headers: (code: number, headers: IncomingHttpHeaders) => {
              status = code;
              recording.headers(headers);
              input.sink.headers(code, headers);
            },

            chunk: async (chunk: Buffer, signal: AbortSignal) => {
              recording.chunk(chunk);
              await input.sink.chunk(chunk, signal);
            },
          },
        };
        const result = bridge
          ? await this.websocket[input.operation === 'compact' ? 'compact' : 'transfer']({
              ...transferInput,
              user: input.user.id,
              provider: input.selection.provider_instance_id,
              binding: input.historyBinding ?? '',
              finish: input.finish,
              failureFrame: (chunk: Buffer) => recording.failureFrame(chunk),
              started: () => {
                upstream = true;
              },
            })
          : await transfer(transferInput);
        if (!bridge) await input.finish?.(signal);
        await recording.settle(result >= 200 && result < 300 ? 'forwarded' : 'upstream_error', result, null);
        return { call_id: recording.row.id, http_status: result };
      } catch (error) {
        const abortReason = transferSignal.reason instanceof DOMException ? transferSignal.reason.message : '';
        const cancellation = [
          'center_shutdown',
          'identity_revoked',
          'bridge_configuration_changed',
          'downstream_closed',
          'test_timeout',
          'test_response_limit',
        ].includes(abortReason)
          ? abortReason
          : 'cancelled';
        const reason = transferSignal.aborted
          ? cancellation
          : error instanceof ControlError
            ? error.code
            : error instanceof WebsocketResponseError
              ? 'response_error'
              : error instanceof TransferError
                ? error.reason
                : transferSignal.aborted
                  ? 'cancelled'
                  : 'center_failure';
        const recordedStatus =
          status ?? (error instanceof ControlError || error instanceof WebsocketResponseError ? error.status : null);
        let outcome: 'upstream_error' | 'interrupted' | 'rejected' =
          upstream || transferSignal.aborted ? 'interrupted' : 'rejected';
        if (error instanceof WebsocketResponseError) {
          outcome = 'upstream_error';
          // 此错误是桥接控制响应；实际失败证据由 failureFrame 独立保存。
        }
        await recording.settle(outcome, recordedStatus, reason);
        throw error;
      } finally {
        this.bridgeRequests.delete(bridgeRequest);
      }
    });
  }

  install(app: FastifyInstance): void {
    const socketOwners = new WeakMap<FastifyRequest['raw']['socket'], FastifyRequest>();
    // 每个请求先恢复管理接口 socket 策略，代理完成正文接收后才由自己的空闲计时接管。
    app.addHook('onRequest', async (request) => {
      socketOwners.set(request.raw.socket, request);
      request.raw.socket.setTimeout?.(10000);
    });
    app.register((scope) => {
      const bodies = new RequestBodies();
      const authenticated = new WeakMap<FastifyRequest, { key: string; user: IdentityUser }>();
      bodies.install(scope);
      scope.setErrorHandler((error, request, reply) => {
        bodies.release(request);
        const failure = error instanceof ControlError ? error : new ControlError('llm_request_invalid');
        if (!request.raw.complete) reply.header('connection', 'close');
        control(reply, request.id, failure);
      });
      scope.addHook('onRequest', async (request) => {
        // 在准入与调用记录创建前拒绝重复、超长或不安全的透传头。
        requestHeaders(request.raw.rawHeaders);
        let key: string | undefined;
        try {
          key = requestCredential(request);
        } catch {
          throw new ControlError('llm_request_invalid');
        }
        if (
          (request.headers['content-encoding'] && request.headers['content-encoding'] !== 'identity') ||
          !/^application\/json(?:\s*;\s*charset\s*=\s*"?utf-8"?\s*)?$/i.test(request.headers['content-type'] ?? '')
        )
          throw new ControlError('llm_request_invalid');
        try {
          const identity = await this.identities.llmIdentity(key ?? '');
          authenticated.set(request, { key: key ?? '', user: identity.user });
        } catch (error) {
          throw new ControlError(
            error instanceof IdentityError && error.status === 401 ? 'llm_key_invalid' : 'llm_proxy_unavailable',
          );
        }
      });
      for (const [suffix, protocol, operation] of [
        ['responses', 'open_ai_responses', 'generation'],
        ['responses/compact', 'open_ai_responses', 'compact'],
        ['responses/input_tokens', 'open_ai_responses', 'input_tokens'],
        ['chat/completions', 'open_ai_chat_completions', 'generation'],
      ] as const) {
        scope.post<{ Params: { id: string } }>(`/api/llm/providers/:id/v1/${suffix}`, async (request, reply) => {
          let body = request.body;
          if (!Buffer.isBuffer(body)) throw new ControlError('llm_request_invalid');
          const parsed = requestModel(body, operation);
          let providerId: string;
          try {
            providerId = uuid(request.params.id);
          } catch {
            throw new ControlError('llm_request_invalid');
          }
          const { key, user } = authenticated.get(request)!;
          authenticated.delete(request);
          const cancelled = new AbortController();

          const closed = () => {
            if (!reply.raw.writableFinished) cancelled.abort(new DOMException('downstream_closed', 'AbortError'));
          };

          reply.raw.once('close', closed);
          request.raw.socket.setTimeout?.(0);
          try {
            await this.execute({
              key,
              user,
              selection: { provider_instance_id: providerId, model_id: parsed.model },
              protocol,
              body,
              signal: cancelled.signal,
              kind: 'proxy',
              requestHeaders: requestHeaders(request.raw.rawHeaders),
              operation,
              historyBinding:
                typeof request.headers['x-ez-center-history-binding'] === 'string'
                  ? request.headers['x-ez-center-history-binding']
                  : undefined,

              admitted: (record) => reply.header('x-ez-call-id', record.row.id),

              sent: () => {
                bodies.release(request);
                body = undefined;
              },

              finish: async (signal) => {
                const finished = once(reply.raw, 'finish', {
                  signal: AbortSignal.any([signal, AbortSignal.timeout(this.options.idleTimeout ?? 300000)]),
                });
                if (!reply.raw.writableEnded) reply.raw.end();
                try {
                  await finished;
                } catch {
                  throw new TransferError(signal.aborted ? 'cancelled' : 'downstream_idle');
                }
              },

              sink: {
                headers: (status, headers) => {
                  reply.hijack();
                  // hijack 绕过 Fastify 的发送流程，保留 CORS 在 onRequest 写入的响应头。
                  for (const [name, value] of Object.entries(reply.getHeaders())) {
                    if (value !== undefined) reply.raw.setHeader(name, value);
                  }
                  reply.raw.writeHead(status, {
                    ...responseHeaders(headers),
                    'x-ez-call-id': String(reply.getHeader('x-ez-call-id')),
                    'cache-control': 'no-store',
                    'x-content-type-options': 'nosniff',
                  });
                },

                chunk: async (chunk: Buffer, signal: AbortSignal) => {
                  if (!reply.raw.write(chunk)) await once(reply.raw, 'drain', { signal });
                },
              },
            });
            if (!reply.raw.writableEnded) reply.raw.end();
          } catch (error) {
            if (reply.raw.headersSent || reply.raw.destroyed) reply.raw.destroy();
            else control(reply, request.id, error);
          } finally {
            bodies.release(request);
            reply.raw.removeListener('close', closed);
            // 文件结算可能晚于下一次 keep-alive 请求，旧调用不得改写新流的 socket 策略。
            if (socketOwners.get(request.raw.socket) === request) request.raw.socket.setTimeout?.(10000);
          }
          return reply;
        });
      }
    });
  }
}
