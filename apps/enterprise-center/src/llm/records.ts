import { randomUUID } from 'node:crypto';
import type { DataSource } from 'typeorm';
import { CallEntity, RecordingEntity } from './entity.js';
import type { Outcome, RecordingMode } from './entity.js';
import { Capture, SnapshotFiles, SnapshotMemory, noSnapshot } from './snapshots.js';
import type { Side, SnapshotResult } from './snapshots.js';
import { ControlError } from './errors.js';
import type { UsageObserver } from './usage.js';
import type { ResponseObserver } from './response-observer.js';
import type { IncomingHttpHeaders } from 'node:http';

const sides = ['request', 'response'] as const;

const diagnostic = () => process.stderr.write('CENTER_LLM_RECORD_FAILURE\n');

/** 网络结果与采集结果分开结算；Promise 由调用 owner 等待，绝不后台重发上游。 */
export class CallRecording {
  readonly request?: Capture;
  readonly response?: Capture;
  private settled?: Promise<void>;
  usage?: UsageObserver;
  observation?: ResponseObserver;
  private received = false;
  private responseBytes = 0;
  private upstreamFailure?: Capture;

  constructor(
    readonly row: CallEntity,
    private readonly records: CallRecords,
    private readonly memory: SnapshotMemory,
    private readonly mode: RecordingMode,
  ) {
    if (mode !== 'off') {
      this.request = new Capture(memory);
      this.response = new Capture(memory);
    }
  }

  headers(headers: IncomingHttpHeaders): void {
    this.received = true;
    // 媒体信息只保存规范化的小值，任意供应商头不进入列表/审计。
    const type = String(headers['content-type'] ?? '')
      .split(';')[0]!
      .trim()
      .toLowerCase();
    const encoding = String(headers['content-encoding'] ?? 'identity')
      .trim()
      .toLowerCase();
    this.row.response_content_type = /^[a-z0-9.+-]+\/[a-z0-9.+-]+$/.test(type) && type.length <= 128 ? type : 'unknown';
    this.row.response_content_encoding = ['identity', 'gzip', 'deflate', 'br', 'zstd'].includes(encoding)
      ? encoding
      : 'unknown';
    this.observation?.headers(headers);
  }

  chunk(chunk: Buffer): void {
    this.responseBytes += chunk.length;
    this.response?.append(chunk);
    this.observation?.chunk(chunk);
  }

  /** 只保留首个未交付失败帧，使用采集共用预算；不混入下游响应正文。 */
  failureFrame(chunk: Buffer): void {
    if (this.settled || this.mode === 'off' || this.upstreamFailure) return;
    this.upstreamFailure = new Capture(this.memory, 1024 * 1024);
    this.upstreamFailure.append(chunk);
  }

  settle(outcome: Outcome, status: number | null, reason: string | null): Promise<void> {
    this.settled ??= this.finish(outcome, status, reason);
    return this.settled;
  }

  private async finish(outcome: Outcome, status: number | null, reason: string | null): Promise<void> {
    try {
      const observed = this.observation?.result(outcome, reason) ?? {
        state: 'anomalous' as const,
        reason: reason ?? 'not_forwarded',
      };
      const retain = this.mode === 'all' || (this.mode === 'anomalies' && observed.state !== 'normal');
      const measured = this.usage?.result(outcome, this.observation?.frames.reason) ?? {
        state: 'unavailable' as const,
        reason:
          this.row.kind === 'input_tokens'
            ? 'input_measurement'
            : outcome === 'rejected'
              ? 'not_forwarded'
              : 'not_reported',
        input_tokens: null,
        output_tokens: null,
        total_tokens: null,
        cached_input_tokens: null,
        reasoning_tokens: null,
      };
      try {
        await this.records.repository.update(this.row.id, {
          outcome,
          response_observation: observed.state,
          response_observation_reason: observed.reason,
          response_source: this.row.response_source ?? null,
          response_content_type: this.row.response_content_type ?? null,
          response_content_encoding: this.row.response_content_encoding ?? null,
          upstream_failure_snapshot: retain && this.upstreamFailure ? noSnapshot('pending') : null,
          http_status: status,
          reason,
          ended_at: new Date(),
          provider_name: this.row.provider_name,
          usage_state: measured.state,
          usage_reason: measured.reason,
          input_tokens: measured.input_tokens,
          output_tokens: measured.output_tokens,
          total_tokens: measured.total_tokens,
          cached_input_tokens: measured.cached_input_tokens,
          reasoning_tokens: measured.reasoning_tokens,
        });
      } catch {
        diagnostic();
      }
      if (outcome === 'interrupted' || outcome === 'unknown') this.response?.interrupt();
      for (const side of [...sides, 'upstream_failure'] as const) {
        const capture = side === 'upstream_failure' ? this.upstreamFailure : this[side];
        if (!capture) continue;
        let result: SnapshotResult;
        try {
          if (!retain) result = noSnapshot('disabled', 'normal_not_retained');
          else if (
            side === 'response' &&
            !this.responseBytes &&
            (!this.received || outcome === 'interrupted' || outcome === 'rejected')
          )
            result = noSnapshot('not_received', reason ?? 'not_forwarded');
          else if (!capture.bytes && capture.reason) result = noSnapshot('failed', capture.reason);
          else result = await this.records.files.publish(this.row.id, this.row.started_at, side, capture);
        } catch {
          result = noSnapshot('failed', 'snapshot_write_failed');
          diagnostic();
        }
        try {
          await this.records.snapshot(this.row.id, side, result);
        } catch {
          diagnostic();
        }
      }
    } finally {
      this.request?.release();
      this.response?.release();
      this.usage?.release();
      this.observation?.release();
      this.upstreamFailure?.release();
      this.records.active.delete(this.row.id);
    }
  }
}

export class CallRecords {
  readonly active = new Set<string>();
  private readonly memory = new SnapshotMemory();
  private timer?: NodeJS.Timeout;
  private cleaning?: Promise<void>;
  private stopped = false;
  private cleanupFailed = false;

  constructor(
    private readonly source: DataSource | undefined,
    readonly files: SnapshotFiles,
  ) {}

  get repository() {
    if (!this.source) throw new ControlError('llm_record_unavailable');
    return this.source.getRepository(CallEntity);
  }

  async settings(): Promise<RecordingEntity> {
    if (!this.source) throw new ControlError('llm_record_unavailable');
    return this.source.getRepository(RecordingEntity).findOneByOrFail({ singleton: true });
  }

  async begin(
    facts: Pick<CallEntity, 'user_id' | 'username' | 'provider_instance_id' | 'model_id' | 'kind' | 'protocol'>,
  ): Promise<CallRecording> {
    try {
      if (this.stopped) throw new ControlError('llm_proxy_unavailable');
      const settings = await this.settings();
      const row = this.repository.create({
        ...facts,
        id: randomUUID(),
        started_at: new Date(),
        ended_at: null,
        provider_name: null,
        outcome: 'in_progress',
        http_status: null,
        reason: null,
        request_snapshot_state: settings.mode !== 'off' ? 'pending' : 'disabled',
        request_snapshot_reason: settings.mode === 'off' ? 'policy_off' : null,
        response_snapshot_state: settings.mode !== 'off' ? 'pending' : 'disabled',
        response_snapshot_reason: settings.mode === 'off' ? 'policy_off' : null,
        response_observation: 'pending',
        usage_state: 'pending',
        usage_reason: null,
      });
      await this.repository.insert(row);
      this.active.add(row.id);
      return new CallRecording(row, this, this.memory, settings.mode);
    } catch {
      throw new ControlError('llm_record_unavailable');
    }
  }

  snapshot(id: string, side: Side, value: SnapshotResult) {
    if (side === 'upstream_failure') return this.repository.update(id, { upstream_failure_snapshot: value });
    return this.repository.update(id, {
      [`${side}_snapshot_state`]: value.state,
      [`${side}_path`]: value.path,
      [`${side}_bytes`]: value.bytes === null ? null : String(value.bytes),
      [`${side}_sha256`]: value.sha256,
      [`${side}_snapshot_reason`]: value.reason,
    });
  }

  async initialize(): Promise<void> {
    if (!this.source) return;
    if ((await this.settings()).mode !== 'off') await this.files.validate();
    // 启动时没有活动请求，逐页恢复；任何存储异常直接停止启动，不连续扩张修复范围。
    let after = '';
    for (;;) {
      const query = this.repository.createQueryBuilder('c').orderBy('c.id', 'ASC').take(100);
      if (after) query.where('c.id > :after', { after });
      const rows = await query.getMany();
      if (!rows.length) break;
      for (const row of rows) {
        if (row.outcome === 'in_progress' || row.usage_state === 'pending' || row.response_observation === 'pending')
          await this.repository.update(row.id, {
            ...(row.outcome === 'in_progress' ? { outcome: 'unknown' as const, reason: 'process_restarted' } : {}),
            ...(row.response_observation === 'pending'
              ? { response_observation: 'unconfirmed' as const, response_observation_reason: 'process_restarted' }
              : {}),
            ...(row.usage_state === 'pending'
              ? { usage_state: 'unavailable' as const, usage_reason: 'process_restarted' }
              : {}),
          });
        for (const side of [...sides, 'upstream_failure'] as const) {
          let meta = snapshotMetadata(row, side);
          if (!meta && side === 'upstream_failure' && row.outcome === 'in_progress') {
            const orphan = await this.files.read(row.id, row.started_at, side);
            if (orphan) {
              await this.snapshot(row.id, side, orphan.result);
              meta = orphan.result;
            }
          }
          if (!meta || !['pending', 'complete', 'partial'].includes(meta.state)) continue;
          const found = await this.files.read(row.id, row.started_at, side);
          if (!found) await this.snapshot(row.id, side, noSnapshot('missing', 'file_missing'));
          else if (meta.state === 'pending') await this.snapshot(row.id, side, found.result);
          else if (found.result.sha256 !== meta.sha256 || found.result.bytes !== meta.bytes)
            await this.snapshot(row.id, side, noSnapshot('failed', 'integrity_mismatch'));
        }
        if (sides.some((side) => row[`${side}_snapshot_state`] !== 'disabled') || row.upstream_failure_snapshot)
          await this.files.clean(row.id, row.started_at, true);
      }
      after = rows.at(-1)!.id;
    }
    this.timer = setInterval(() => {
      if (this.cleaning || this.stopped || this.cleanupFailed) return;
      this.cleaning = this.cleanup()
        .catch(() => {
          this.cleanupFailed = true;
          clearInterval(this.timer);
          diagnostic();
        })
        .finally(() => {
          this.cleaning = undefined;
        });
    }, 3600000);
    this.timer.unref();
  }

  async cleanup(): Promise<void> {
    const settings = await this.settings();
    const now = Date.now();
    const contentBefore = new Date(now - settings.content_retention_days * 86400000);
    const indexBefore = new Date(now - settings.index_retention_days * 86400000);
    const rows = await this.repository
      .createQueryBuilder('c')
      .where(
        "c.outcome <> 'in_progress' AND c.request_snapshot_state <> 'pending' AND c.response_snapshot_state <> 'pending'",
      )
      .andWhere("(CASE WHEN c.outcome = 'unknown' THEN c.started_at ELSE c.ended_at END) < :before", {
        before: contentBefore,
      })
      .andWhere(
        "(c.request_snapshot_state NOT IN ('disabled','expired','not_received') OR c.response_snapshot_state NOT IN ('disabled','expired','not_received') OR c.upstream_failure_snapshot->>'state' NOT IN ('disabled','expired','not_received') OR (CASE WHEN c.outcome = 'unknown' THEN c.started_at ELSE c.ended_at END) < :indexBefore)",
        { indexBefore },
      )
      .orderBy('c.started_at', 'ASC')
      .take(100)
      .getMany();
    for (const row of rows) {
      if (this.active.has(row.id)) continue;
      if (sides.some((side) => row[`${side}_snapshot_state`] !== 'disabled') || row.upstream_failure_snapshot)
        await this.files.clean(row.id, row.started_at);
      const ended = row.outcome === 'unknown' ? row.started_at : row.ended_at!;
      if (ended < indexBefore) await this.repository.delete(row.id);
      else
        for (const side of [...sides, 'upstream_failure'] as const) {
          const state = snapshotMetadata(row, side)?.state;
          if (state && !['disabled', 'not_received'].includes(state))
            await this.snapshot(row.id, side, noSnapshot('expired'));
        }
    }
  }

  async close(): Promise<void> {
    this.stopped = true;
    clearInterval(this.timer);
    await this.cleaning;
  }
}

/** 数据库小元数据与文件寻址分离；失败帧缺失不伪造两侧正文。 */
export function snapshotMetadata(row: CallEntity, side: Side): SnapshotResult | null {
  if (side === 'upstream_failure') return row.upstream_failure_snapshot ?? null;
  return {
    state: row[`${side}_snapshot_state`],
    path: row[`${side}_path`],
    bytes: row[`${side}_bytes`] == null ? null : Number(row[`${side}_bytes`]),
    sha256: row[`${side}_sha256`],
    reason: row[`${side}_snapshot_reason`],
  };
}
