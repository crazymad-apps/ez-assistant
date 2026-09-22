import { randomUUID } from 'node:crypto';
import type { DataSource } from 'typeorm';
import { CallEntity, RecordingEntity } from './entity.js';
import type { Outcome } from './entity.js';
import { Capture, SnapshotFiles, SnapshotMemory, noSnapshot } from './snapshots.js';
import type { Side, SnapshotResult } from './snapshots.js';
import { ControlError } from './errors.js';

const sides = ['request', 'response'] as const;

const diagnostic = () => process.stderr.write('CENTER_LLM_RECORD_FAILURE\n');

/** 网络结果与采集结果分开结算；Promise 由调用 owner 等待，绝不后台重发上游。 */
export class CallRecording {
  readonly request?: Capture;
  readonly response?: Capture;
  private settled?: Promise<void>;
  responseUnsupported = false;

  constructor(
    readonly row: CallEntity,
    private readonly records: CallRecords,
    memory: SnapshotMemory,
    enabled: boolean,
  ) {
    if (enabled) {
      this.request = new Capture(memory);
      this.response = new Capture(memory);
    }
  }

  settle(outcome: Outcome, status: number | null, reason: string | null): Promise<void> {
    this.settled ??= this.finish(outcome, status, reason);
    return this.settled;
  }

  private async finish(outcome: Outcome, status: number | null, reason: string | null): Promise<void> {
    try {
      try {
        await this.records.repository.update(this.row.id, {
          outcome,
          http_status: status,
          reason,
          ended_at: new Date(),
          provider_name: this.row.provider_name,
        });
      } catch {
        diagnostic();
      }
      if (outcome === 'interrupted' || outcome === 'unknown') this.response?.interrupt();
      for (const side of sides) {
        const capture = this[side];
        if (!capture) continue;
        let result: SnapshotResult;
        try {
          if (side === 'response' && outcome === 'rejected') result = noSnapshot('failed', 'not_forwarded');
          else if (side === 'response' && this.responseUnsupported)
            result = noSnapshot('failed', 'unsupported_encoding');
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
        request_snapshot_state: settings.enabled ? 'pending' : 'disabled',
        response_snapshot_state: settings.enabled ? 'pending' : 'disabled',
      });
      await this.repository.insert(row);
      this.active.add(row.id);
      return new CallRecording(row, this, this.memory, settings.enabled);
    } catch {
      throw new ControlError('llm_record_unavailable');
    }
  }

  snapshot(id: string, side: Side, value: SnapshotResult) {
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
    if ((await this.settings()).enabled) await this.files.validate();
    // 启动时没有活动请求，逐页恢复；任何存储异常直接停止启动，不连续扩张修复范围。
    let after = '';
    for (;;) {
      const query = this.repository.createQueryBuilder('c').orderBy('c.id', 'ASC').take(100);
      if (after) query.where('c.id > :after', { after });
      const rows = await query.getMany();
      if (!rows.length) break;
      for (const row of rows) {
        if (row.outcome === 'in_progress')
          await this.repository.update(row.id, { outcome: 'unknown', reason: 'process_restarted' });
        for (const side of sides) {
          const state = row[`${side}_snapshot_state`];
          if (!['pending', 'complete', 'partial'].includes(state)) continue;
          const found = await this.files.read(row.id, row.started_at, side);
          if (!found) await this.snapshot(row.id, side, noSnapshot('missing', 'file_missing'));
          else if (state === 'pending') {
            let recovered = found.result;
            try {
              new TextDecoder('utf-8', { fatal: true }).decode(found.body);
            } catch {
              recovered = noSnapshot('failed', 'invalid_utf8');
            }
            await this.snapshot(row.id, side, recovered);
          } else if (
            found.result.sha256 !== row[`${side}_sha256`] ||
            String(found.result.bytes) !== row[`${side}_bytes`]
          ) {
            await this.snapshot(row.id, side, noSnapshot('failed', 'integrity_mismatch'));
          }
        }
        if (sides.some((side) => row[`${side}_snapshot_state`] !== 'disabled'))
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
        "(c.request_snapshot_state NOT IN ('disabled','expired') OR c.response_snapshot_state NOT IN ('disabled','expired') OR (CASE WHEN c.outcome = 'unknown' THEN c.started_at ELSE c.ended_at END) < :indexBefore)",
        { indexBefore },
      )
      .orderBy('c.started_at', 'ASC')
      .take(100)
      .getMany();
    for (const row of rows) {
      if (this.active.has(row.id)) continue;
      if (sides.some((side) => row[`${side}_snapshot_state`] !== 'disabled'))
        await this.files.clean(row.id, row.started_at);
      const ended = row.outcome === 'unknown' ? row.started_at : row.ended_at!;
      if (ended < indexBefore) await this.repository.delete(row.id);
      else
        for (const side of sides)
          if (row[`${side}_snapshot_state`] !== 'disabled') await this.snapshot(row.id, side, noSnapshot('expired'));
    }
  }

  async close(): Promise<void> {
    this.stopped = true;
    clearInterval(this.timer);
    await this.cleaning;
  }
}
