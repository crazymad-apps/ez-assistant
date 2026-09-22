import { once } from 'node:events';
import type { FastifyInstance, FastifyReply, FastifyRequest } from 'fastify';
import type { IdentityService } from '../identity/service.js';
import { IdentityError } from '../identity/errors.js';
import { requestCredential } from '../security.js';
import type { ModelsService } from '../models/service.js';
import type { ModelSelection, Protocol } from '../contracts/models.js';
import type { IdentityUser } from '../contracts/identity.js';
import { uuid } from '../models/parameters.js';
import { RequestBodies, requestModel } from './body.js';
import { CallRecords } from './records.js';
import type { CallRecording } from './records.js';
import { ControlError } from './errors.js';
import { transfer, responseHeaders, TransferError } from './transport.js';
import type { TransferSink } from './transport.js';

export type ProxyOptions = { origin?: string; connectTimeout?: number; idleTimeout?: number };

function control(reply: FastifyReply, requestId: string, error: unknown): void {
  const failure = error instanceof ControlError ? error : new ControlError('llm_proxy_unavailable');
  void reply
    .header('x-ez-center-control', '1')
    .header('cache-control', 'no-store')
    .header('x-content-type-options', 'nosniff')
    .status(failure.status)
    .send({ error: { code: failure.code, message: failure.message, request_id: requestId } });
}

/** 服务 owner 跟踪从准入到文件结算的整个 Promise，关闭先取消网络再等待记录。 */
export class ModelProxy {
  private readonly shutdown = new AbortController();
  private readonly pending = new Set<Promise<unknown>>();

  constructor(
    private readonly identities: IdentityService,
    private readonly models: ModelsService,
    readonly records: CallRecords,
    private readonly options: ProxyOptions,
  ) {}

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
    this.shutdown.abort();
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
        kind: input.kind,
        protocol: input.protocol,
      });
      input.admitted?.(recording);
      recording.request?.append(input.body);
      let status: number | null = null;
      let upstream = false;
      try {
        const route = await this.models.admit(input.key, input.selection, input.protocol, input.kind === 'admin_test');
        signal.throwIfAborted();
        recording.row.provider_name = route.provider.connection.display_name;
        signal.throwIfAborted();
        upstream = true;
        const result = await transfer({
          endpoint: route.provider.connection.endpoint,
          protocol: input.protocol,
          apiKey: route.provider.api_key,
          body: input.body,
          signal,
          connectTimeout: this.options.connectTimeout ?? 10000,
          idleTimeout: this.options.idleTimeout ?? 300000,

          sent: () => {
            input.body = Buffer.alloc(0);
            input.sent?.();
          },

          sink: {
            headers: (code, headers) => {
              status = code;
              recording.responseUnsupported =
                !!headers['content-encoding'] && headers['content-encoding'] !== 'identity';
              input.sink.headers(code, headers);
            },

            chunk: async (chunk, signal) => {
              if (!recording.responseUnsupported) recording.response?.append(chunk);
              await input.sink.chunk(chunk, signal);
            },
          },
        });
        await input.finish?.(signal);
        await recording.settle(result >= 200 && result < 300 ? 'forwarded' : 'upstream_error', result, null);
        return { call_id: recording.row.id, http_status: result };
      } catch (error) {
        const reason =
          error instanceof ControlError
            ? error.code
            : error instanceof TransferError
              ? error.reason
              : signal.aborted
                ? 'cancelled'
                : 'center_failure';
        const recordedStatus = status ?? (error instanceof ControlError ? error.status : null);
        await recording.settle(upstream || signal.aborted ? 'interrupted' : 'rejected', recordedStatus, reason);
        throw error;
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
        let key: string | undefined;
        try {
          key = requestCredential(request, this.options.origin);
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
      for (const [suffix, protocol] of [
        ['responses', 'open_ai_responses'],
        ['chat/completions', 'open_ai_chat_completions'],
      ] as const) {
        scope.post<{ Params: { id: string } }>(`/api/llm/providers/:id/v1/${suffix}`, async (request, reply) => {
          let body = request.body;
          if (!Buffer.isBuffer(body)) throw new ControlError('llm_request_invalid');
          const parsed = requestModel(body);
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
            if (!reply.raw.writableFinished) cancelled.abort();
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
                  reply.raw.writeHead(status, {
                    ...responseHeaders(headers),
                    'x-ez-call-id': String(reply.getHeader('x-ez-call-id')),
                    'cache-control': 'no-store',
                    'x-content-type-options': 'nosniff',
                  });
                },

                chunk: async (chunk, signal) => {
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
