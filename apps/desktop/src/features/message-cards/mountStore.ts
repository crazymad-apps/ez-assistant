import { action, makeObservable, observable, observableRef, runInAction } from 'mobx';
import type {
  ConversationOwner,
  MessageCardSnapshot,
  MountMessageCardResult,
  SaveMessageCardSnapshotResult,
} from '@ez-assistant/protocol';
import type { RuntimeClient } from '../../runtime-client/RuntimeClient';

/** 单张可见卡片的短期投影；句柄只留在受信任客户端，不交给内嵌页面。 */
export class CardMountStore {
  mounted: MountMessageCardResult | null = null;
  pending = false;
  error: string | null = null;
  save_error: string | null = null;
  #request: AbortController | null = null;

  constructor(
    readonly client: RuntimeClient,
    readonly owner: ConversationOwner,
    readonly card_id: string,
  ) {
    makeObservable(this, {
      mounted: observableRef,
      pending: observable,
      error: observable,
      save_error: observable,
      load: action,
      dispose: action,
    });
  }

  async load(): Promise<void> {
    this.dispose();
    const request = new AbortController();
    this.#request = request;
    this.pending = true;
    this.error = null;
    try {
      const result = await this.client.command(
        {
          type: 'mount_message_card',
          payload: { owner: this.owner, card_id: this.card_id, client_instance_id: this.client.ui_client_instance_id },
        },
        { signal: request.signal },
      );
      if (this.#request !== request) {
        this.release(result.payload.instance_id);
        return;
      }
      runInAction(() => {
        this.mounted = result.payload;
      });
    } catch {
      if (this.#request === request)
        runInAction(() => {
          this.error = '卡片不可用：入口缺失、配置不全或快照不兼容。';
        });
    } finally {
      if (this.#request === request)
        runInAction(() => {
          this.pending = false;
        });
    }
  }

  async save(
    instance_id: string,
    expected_revision: number,
    snapshot: MessageCardSnapshot,
  ): Promise<SaveMessageCardSnapshotResult> {
    if (this.mounted?.instance_id !== instance_id) throw new Error('卡片已失效');
    try {
      const result = await this.client.command({
        type: 'save_message_card_snapshot',
        payload: { owner: this.owner, card_id: this.card_id, instance_id, expected_revision, snapshot },
      });
      if (this.mounted?.instance_id !== instance_id) throw new Error('卡片已失效');
      runInAction(() => {
        this.save_error = result.payload.saved ? null : '快照已在其他页面更新，请重新加载后处理冲突。';
      });
      return result.payload;
    } catch {
      if (this.mounted?.instance_id === instance_id)
        runInAction(() => {
          this.save_error = '快照保存失败，上次成功状态仍保留。请重新加载核对。';
        });
      throw new Error('快照未保存');
    }
  }

  release(instance_id: string): void {
    void this.client
      .command({ type: 'release_message_card', payload: { owner: this.owner, card_id: this.card_id, instance_id } })
      .catch(() => undefined);
  }

  dispose(): void {
    this.#request?.abort();
    this.#request = null;
    if (this.mounted) this.release(this.mounted.instance_id);
    this.mounted = null;
    this.pending = false;
    this.save_error = null;
  }
}
