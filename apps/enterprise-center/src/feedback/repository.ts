import type { EntityManager } from 'typeorm';
import type { FeedbackContent, FeedbackDetail, FeedbackPage, FeedbackSummary } from '../contracts/feedback.js';
import { FeedbackEntity } from './entity.js';
import type { feedbackQuery } from './validation.js';

export function content(row: FeedbackEntity): FeedbackContent {
  return { target: { kind: row.target_kind, reference: row.target_reference, name: row.target_name,
    version: row.target_version, content_ref: row.target_content_ref, location: row.target_location },
  title: row.title, description: row.description, reproduction: row.reproduction, expected_result: row.expected_result };
}
export function detail(row: FeedbackEntity): FeedbackDetail {
  return { ...content(row), id: row.id, submission_id: row.submission_id, source: row.source,
    target_kind: row.target_kind, target_name: row.target_name, status: row.status,
    owner_user_id: row.owner_user_id, owner_username: row.owner_username, owner_display_name: row.owner_display_name,
    environment: row.environment, handling_note: row.handling_note, handler_user_id: row.handler_user_id,
    handler_username: row.handler_username, handler_display_name: row.handler_display_name,
    created_at: row.created_at.toISOString(), updated_at: row.updated_at.toISOString() };
}

/** 列表与总数使用同一语句快照；不返回正文、环境和完整目标引用。 */
export async function listFeedback(manager: EntityManager, input: ReturnType<typeof feedbackQuery>, owner: number | null): Promise<FeedbackPage> {
  const rows: { items: FeedbackSummary[]; total: number }[] = await manager.query(`WITH filtered AS (
    SELECT id,title,target_kind,target_name,status,source,owner_user_id,owner_username,owner_display_name,created_at,updated_at
    FROM public.feedback WHERE ($1::integer IS NULL OR owner_user_id=$1)
      AND ($2::text IS NULL OR status=$2) AND ($3::text IS NULL OR source=$3)
      AND ($4::text IS NULL OR target_kind=$4) AND ($5::text IS NULL OR target_reference=$5)
      AND ($6::boolean IS NULL OR (target_reference IS NULL)=$6)
      AND ($7::text IS NULL OR strpos(lower(title),lower($7))>0)
      AND ($8::uuid IS NULL OR submission_id=$8)
  ), page AS (SELECT * FROM filtered ORDER BY created_at DESC,id DESC LIMIT $9 OFFSET $10)
  SELECT (SELECT count(*)::integer FROM filtered) AS total,
    COALESCE((SELECT json_agg(page ORDER BY created_at DESC,id DESC) FROM page),'[]'::json) AS items`,
  [owner, input.status, input.source, input.kind, input.reference, input.unresolved, input.title, input.submission, input.limit, input.offset]);
  return { ...rows[0]!, limit: input.limit, offset: input.offset };
}
