import { randomUUID } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import type { DataSource } from 'typeorm';
import { IdentityService } from '../identity/service.js';
import { IdentityError } from '../identity/errors.js';
import { FeedbackEntity } from './entity.js';
import { content, detail, listFeedback } from './repository.js';
import { feedbackId, feedbackQuery, handling, submission } from './validation.js';

export class FeedbackService {
  constructor(private readonly identity: IdentityService, private readonly source?: DataSource) {}
  private get manager() {
    if (!this.source?.isInitialized) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    return this.source.manager;
  }
  async submit(token: string | undefined, value: unknown, requestId: string) {
    const input = submission(value);
    return this.identity.write(async manager => {
      const owner = await this.identity.requireUser(manager, token);
      const repo = manager.getRepository(FeedbackEntity);
      const existing = await repo.findOneBy({ owner_user_id: owner.id, submission_id: input.submission_id });
      if (existing) {
        const { submission_id: _id, environment: _environment, source, ...body } = input;
        if (existing.source !== source || !isDeepStrictEqual(content(existing), body))
          throw new IdentityError(409, 'FEEDBACK_SUBMISSION_CONFLICT');
        return detail(existing);
      }
      const row = repo.create({ id: randomUUID(), owner_user_id: owner.id, owner_username: owner.username,
        owner_display_name: owner.display_name, submission_id: input.submission_id, source: input.source,
        target_kind: input.target.kind, target_reference: input.target.reference, target_name: input.target.name,
        target_version: input.target.version, target_content_ref: input.target.content_ref, target_location: input.target.location,
        title: input.title, description: input.description, reproduction: input.reproduction, expected_result: input.expected_result,
        environment: input.environment, status: 'pending', handling_note: null, handler_user_id: null,
        handler_username: null, handler_display_name: null });
      await repo.insert(row);
      await this.identity.audit(manager, 'feedback_created', requestId, owner.id, undefined, owner.id,
        { feedback_id: row.id, status: 'pending' });
      return detail((await repo.findOneByOrFail({ id: row.id })));
    });
  }
  async list(token: string | undefined, value: unknown, admin: boolean) {
    const input = feedbackQuery(value, admin);
    const manager = this.manager;
    const owner = admin ? await this.identity.requireAdmin(manager, token) : await this.identity.requireUser(manager, token);
    const result = await listFeedback(manager, input, admin ? input.owner : owner.id);
    // 查询期间可能注销或降权，异步结果发布前重验。
    if (admin) await this.identity.requireAdmin(manager, token);
    else await this.identity.requireUser(manager, token);
    return result;
  }
  async get(token: string | undefined, id: string, admin: boolean) {
    const key = feedbackId(id);
    const manager = this.manager;
    const owner = admin ? await this.identity.requireAdmin(manager, token) : await this.identity.requireUser(manager, token);
    const row = await manager.getRepository(FeedbackEntity).findOneBy({ id: key, ...(admin ? {} : { owner_user_id: owner.id }) });
    if (admin) await this.identity.requireAdmin(manager, token);
    else await this.identity.requireUser(manager, token);
    if (!row) throw new IdentityError(404, 'FEEDBACK_NOT_FOUND');
    return detail(row);
  }
  async handle(token: string | undefined, id: string, value: unknown, requestId: string) {
    const key = feedbackId(id);
    const input = handling(value);
    return this.identity.write(async manager => {
      const actor = await this.identity.requireAdmin(manager, token);
      const repo = manager.getRepository(FeedbackEntity);
      const row = await repo.findOneBy({ id: key });
      if (!row) throw new IdentityError(404, 'FEEDBACK_NOT_FOUND');
      if (row.status === input.status && row.handling_note === input.handling_note) return detail(row);
      if (row.status !== input.expected_status || row.handling_note !== input.expected_handling_note)
        throw new IdentityError(409, 'FEEDBACK_CHANGED');
      await repo.update({ id: key }, { status: input.status, handling_note: input.handling_note,
        handler_user_id: actor.id, handler_username: actor.username, handler_display_name: actor.display_name,
        updated_at: () => 'CURRENT_TIMESTAMP' });
      await this.identity.audit(manager, 'feedback_handled', requestId, actor.id, undefined, row.owner_user_id,
        { feedback_id: key, previous_status: row.status, status: input.status });
      return detail(await repo.findOneByOrFail({ id: key }));
    });
  }
}
