import type { EntityManager } from 'typeorm';
import { AuditEntity } from './entity.js';
import type { AuditRecord } from '../contracts/audit.js';
import type { auditQuery } from './service.js';

export const audits = {
  async append(manager: EntityManager, action: string, requestId: string, userId: number | null,
    reason?: string, targetId = userId, details: object = {}): Promise<void> {
    await manager.getRepository(AuditEntity).insert({ actor_user_id: userId,
      target_user_id: targetId, action, success: !reason, reason_code: reason ?? null, request_id: requestId, details });
  },
  async list(manager: EntityManager, input: ReturnType<typeof auditQuery>) {
    // 同一快照中先分页再关联用户名；LEFT JOIN 保留未认证/无目标记录，不新增姓名快照或逐行查询。
    const rows: { total: number; items: AuditRecord[] }[] = await manager.query(`WITH filtered AS (
      SELECT id,occurred_at,actor_user_id,target_user_id,action,success,reason_code,request_id,details
      FROM public.management_audit WHERE ($1::timestamptz IS NULL OR occurred_at >= $1)
      AND ($2::timestamptz IS NULL OR occurred_at < $2) AND ($3::integer IS NULL OR actor_user_id=$3)
      AND ($4::text IS NULL OR action=$4) AND ($5::boolean IS NULL OR success=$5)
      AND ($8::integer IS NULL OR id=$8)
    ), page AS (SELECT * FROM filtered ORDER BY occurred_at DESC,id DESC LIMIT $6 OFFSET $7),
    named_page AS (
      SELECT page.*, actor.username AS actor_username, target.username AS target_username
      FROM page LEFT JOIN public.users actor ON actor.id=page.actor_user_id
      LEFT JOIN public.users target ON target.id=page.target_user_id
    )
    SELECT (SELECT count(*)::integer FROM filtered) AS total,
      COALESCE((SELECT json_agg(named_page ORDER BY occurred_at DESC,id DESC) FROM named_page),'[]'::json) AS items`,
    [input.from ?? null, input.to ?? null, input.actor_user_id ?? null, input.action ?? null, input.success ?? null, input.limit, input.offset, input.id ?? null]);
    return rows[0]!;
  },
};
